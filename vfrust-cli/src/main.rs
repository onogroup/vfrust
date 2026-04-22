mod cli;
mod cloudinit;
mod parse;
mod timesync;

use std::fs;
use std::path::PathBuf;

use clap::Parser;
use vfrust::config::bootloader::{Bootloader, LinuxBootloader};
use vfrust::config::device::gpu::VirtioGpu;
use vfrust::config::device::input::VirtioInput;
use vfrust::config::device::storage::UsbMassStorage;
use vfrust::config::device::vsock::VirtioVsock;
use vfrust::config::device::Device;
use vfrust::{VirtualMachine, VmConfig};

/// Format the current time as `HH:MM:SS` in UTC.  Zero new deps.
fn chrono_like_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Guard that removes a PID file when dropped.
struct PidFileGuard {
    path: PathBuf,
}

impl PidFileGuard {
    fn new(path: &str) -> Result<Self, String> {
        let pid = std::process::id();
        fs::write(path, pid.to_string())
            .map_err(|e| format!("failed to write PID file '{path}': {e}"))?;
        tracing::info!("wrote PID {} to {}", pid, path);
        Ok(Self {
            path: PathBuf::from(path),
        })
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            tracing::warn!("failed to remove PID file '{}': {}", self.path.display(), e);
        } else {
            tracing::debug!("removed PID file '{}'", self.path.display());
        }
    }
}

fn main() {
    // Ignore SIGPIPE to avoid unexpected termination when piping output
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let cli = cli::Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cli.log_level)),
        )
        .init();

    let bootloader = if let Some(ref kernel) = cli.kernel {
        tracing::warn!("--kernel/--initrd/--kernel-cmdline are deprecated, use --bootloader linux,...");
        Bootloader::Linux(LinuxBootloader {
            kernel_path: PathBuf::from(kernel),
            initrd_path: cli.initrd.as_ref().map(PathBuf::from),
            command_line: cli.kernel_cmdline.clone().unwrap_or_default(),
        })
    } else if let Some(ref bootloader_spec) = cli.bootloader {
        parse::parse_bootloader(bootloader_spec).unwrap_or_else(|e| {
            eprintln!("error parsing bootloader: {e}");
            std::process::exit(1);
        })
    } else {
        eprintln!("error: either --bootloader or --kernel must be specified");
        std::process::exit(1);
    };

    let mut devices: Vec<Device> = cli
        .device
        .iter()
        .map(|d| parse::parse_device(d))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| {
            eprintln!("error parsing device: {e}");
            std::process::exit(1);
        });

    // If --gui, auto-add GPU + input devices if not already present
    if cli.gui {
        let has_gpu = devices.iter().any(|d| matches!(d, Device::VirtioGpu(_)));
        let has_keyboard = devices
            .iter()
            .any(|d| matches!(d, Device::VirtioInput(VirtioInput::Keyboard)));
        let has_pointing = devices
            .iter()
            .any(|d| matches!(d, Device::VirtioInput(VirtioInput::Pointing)));

        if !has_gpu {
            devices.push(Device::VirtioGpu(VirtioGpu::default()));
        }
        if !has_keyboard {
            devices.push(Device::VirtioInput(VirtioInput::Keyboard));
        }
        if !has_pointing {
            devices.push(Device::VirtioInput(VirtioInput::Pointing));
        }
    }

    // Auto-inject vsock device if --timesync is specified and none exists
    if cli.timesync.is_some() {
        let has_vsock = devices.iter().any(|d| matches!(d, Device::VirtioVsock(_)));
        if !has_vsock {
            devices.push(Device::VirtioVsock(VirtioVsock {
                port: 0, // port is managed by the timesync module
                socket_url: None,
                listen: false,
            }));
            tracing::info!("auto-added virtio-vsock device for timesync");
        }
    }

    // Cloud-init: generate ISO and attach as USB mass storage
    let _cloud_init_iso: Option<PathBuf> = if let Some(ref cloud_init_arg) = cli.cloud_init {
        let file_paths: Vec<&str> = cloud_init_arg.split(',').collect();
        let iso_path = cloudinit::generate_cloud_init_iso(&file_paths).unwrap_or_else(|e| {
            eprintln!("error generating cloud-init ISO: {e}");
            std::process::exit(1);
        });
        tracing::info!("cloud-init ISO created at {}", iso_path.display());
        devices.push(Device::UsbMassStorage(UsbMassStorage {
            path: iso_path.clone(),
            read_only: true,
        }));
        Some(iso_path)
    } else {
        None
    };

    // TODO: --ignition support requires vsock proxy implementation (Phase C4-C6).
    // The ignition config would be served to the VM guest over vsock.
    if cli.ignition.is_some() {
        tracing::warn!("--ignition is not yet implemented; requires vsock proxy support");
    }

    let timesync_port = cli.timesync;
    let metrics_interval_secs = cli.metrics_interval;

    // PID file (create before VM, cleaned up on drop)
    let _pid_guard = cli.pidfile.as_ref().map(|path| {
        PidFileGuard::new(path).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        })
    });

    let config = VmConfig::builder()
        .cpus(cli.cpus)
        .memory_mib(cli.memory)
        .bootloader(bootloader)
        .devices(devices)
        .nested(cli.nested)
        .build()
        .unwrap_or_else(|e| {
            eprintln!("error building VM config: {e}");
            std::process::exit(1);
        });

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

    // Collect vsock proxy configs before the config is consumed
    let vsock_proxies: Vec<_> = config
        .devices()
        .iter()
        .filter_map(|d| {
            if let Device::VirtioVsock(vsock_cfg) = d {
                vsock_cfg.socket_url.as_ref().map(|url| {
                    (vsock_cfg.port, url.clone(), vsock_cfg.listen)
                })
            } else {
                None
            }
        })
        .collect();

    rt.block_on(async {
        let vm = VirtualMachine::new(config).unwrap_or_else(|e| {
            eprintln!("error creating VM: {e}");
            std::process::exit(1);
        });

        let handle = vm.handle();

        tracing::info!("starting VM...");
        if let Err(e) = handle.start().await {
            eprintln!("error starting VM: {e}");
            std::process::exit(1);
        }
        tracing::info!("VM started");

        // Start time synchronization if requested
        if let Some(port) = timesync_port {
            timesync::start_timesync(handle.clone(), port, tokio::runtime::Handle::current());
        }

        // Periodically print host-observed resource usage if requested.
        if let Some(secs) = metrics_interval_secs {
            let secs = secs.max(1);
            let handle_metrics = handle.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
                // Skip the immediate first tick so the worker has time to fork.
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    match handle_metrics.resource_usage() {
                        Some(u) => {
                            let ts = chrono_like_timestamp();
                            println!(
                                "[{ts}] pid={} {u}",
                                handle_metrics.worker_pid().unwrap_or(0),
                            );
                        }
                        None => {
                            tracing::debug!("metrics sample: worker not yet available");
                        }
                    }
                }
            });
        }

        // Set up vsock proxies from device configuration
        for (port, socket_url, listen) in &vsock_proxies {
            let handle_clone = handle.clone();
            let port = *port;
            let path = PathBuf::from(socket_url);
            let listen = *listen;
            tokio::spawn(async move {
                let result = if listen {
                    // Listen mode: guest connects to vsock port, proxied to host unix socket
                    match vfrust::vsock::listen_vsock(&handle_clone, port).await {
                        Ok(mut rx) => {
                            tracing::info!(
                                "vsock listening on port {port}, proxying to {}",
                                path.display()
                            );
                            while let Some(conn) = rx.recv().await {
                                let path = path.clone();
                                std::thread::spawn(move || {
                                    if let Err(e) = proxy_vsock_to_unix(conn, &path) {
                                        tracing::warn!("vsock proxy error: {e}");
                                    }
                                });
                            }
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    // Connect mode: host listens on unix socket, proxies to guest vsock
                    setup_connect_proxy(&handle_clone, port, &path).await
                };
                if let Err(e) = result {
                    tracing::error!("vsock proxy setup failed for port {port}: {e}");
                }
            });
        }

        // Wait for ctrl-c or SIGTERM
        let shutdown = async {
            let ctrl_c = tokio::signal::ctrl_c();
            let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");

            tokio::select! {
                _ = ctrl_c => {
                    tracing::info!("received SIGINT, shutting down...");
                }
                _ = sigterm.recv() => {
                    tracing::info!("received SIGTERM, shutting down...");
                }
            }
        };
        shutdown.await;

        if let Err(e) = handle.request_stop().await {
            tracing::warn!("graceful stop failed: {e}, force stopping...");
            if let Err(e) = handle.stop().await {
                eprintln!("force stop failed: {e}");
            }
        }

        tracing::info!("VM stopped");
    });

    // Clean up cloud-init ISO if we created one
    if let Some(ref iso_path) = _cloud_init_iso {
        if let Err(e) = fs::remove_file(iso_path) {
            tracing::warn!("failed to clean up cloud-init ISO: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Vsock proxy helpers
// ---------------------------------------------------------------------------

/// Bidirectionally proxy data between a vsock connection and a unix socket.
/// Blocks until either side closes or an error occurs.
fn proxy_vsock_to_unix(
    conn: vfrust::VsockConnection,
    unix_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::os::unix::net::UnixStream;

    let unix_stream = UnixStream::connect(unix_path)?;
    let unix_clone = unix_stream.try_clone()?;

    // Wrap conn in Arc so both threads can access it
    let conn = std::sync::Arc::new(conn);
    let conn2 = conn.clone();

    // vsock -> unix
    let handle_v2u = std::thread::spawn(move || {
        use std::io::Write;
        let mut buf = [0u8; 8192];
        loop {
            match conn.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if (&unix_stream).write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // Shut down write side of unix socket to signal EOF
        let _ = unix_stream.shutdown(std::net::Shutdown::Write);
    });

    // unix -> vsock
    let handle_u2v = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        loop {
            match (&unix_clone).read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let mut offset = 0;
                    while offset < n {
                        match conn2.write(&buf[offset..n]) {
                            Ok(0) => return,
                            Ok(w) => offset += w,
                            Err(_) => return,
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    let _ = handle_v2u.join();
    let _ = handle_u2v.join();
    Ok(())
}

/// Connect mode: listen on a unix socket, for each incoming connection, connect
/// to the guest via vsock and proxy bidirectionally.
async fn setup_connect_proxy(
    handle: &vfrust::VmHandle,
    port: u32,
    unix_path: &std::path::Path,
) -> Result<(), vfrust::Error> {
    use std::os::unix::net::UnixListener;

    // Remove stale socket if present
    let _ = std::fs::remove_file(unix_path);

    let listener = UnixListener::bind(unix_path).map_err(|e| {
        vfrust::Error::InvalidDevice(format!(
            "failed to bind unix socket at {}: {e}",
            unix_path.display()
        ))
    })?;

    tracing::info!(
        "vsock connect proxy: listening on {}, forwarding to guest port {port}",
        unix_path.display()
    );

    let handle = handle.clone();
    let unix_path_owned = unix_path.to_path_buf();

    // Accept loop in a blocking thread
    tokio::task::spawn_blocking(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(unix_stream) => {
                    let handle = handle.clone();
                    let unix_path = unix_path_owned.clone();
                    std::thread::spawn(move || {
                        // We need a runtime to call connect_vsock (async)
                        let rt = match tokio::runtime::Handle::try_current() {
                            Ok(h) => h,
                            Err(_) => {
                                tracing::error!("no tokio runtime for vsock connect");
                                return;
                            }
                        };
                        let conn = match rt.block_on(vfrust::vsock::connect_vsock(&handle, port)) {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!(
                                    "failed to connect vsock port {port}: {e}"
                                );
                                return;
                            }
                        };
                        let conn = std::sync::Arc::new(conn);
                        let conn2 = conn.clone();
                        let unix_clone = match unix_stream.try_clone() {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("failed to clone unix stream: {e}");
                                return;
                            }
                        };

                        // vsock -> unix
                        let h1 = std::thread::spawn(move || {
                            use std::io::Write;
                            let mut buf = [0u8; 8192];
                            loop {
                                match conn.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        if (&unix_stream).write_all(&buf[..n]).is_err() {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            let _ = unix_stream.shutdown(std::net::Shutdown::Write);
                        });

                        // unix -> vsock
                        let h2 = std::thread::spawn(move || {
                            use std::io::Read;
                            let mut buf = [0u8; 8192];
                            loop {
                                match (&unix_clone).read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        let mut offset = 0;
                                        while offset < n {
                                            match conn2.write(&buf[offset..n]) {
                                                Ok(0) => return,
                                                Ok(w) => offset += w,
                                                Err(_) => return,
                                            }
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        });

                        let _ = h1.join();
                        let _ = h2.join();
                        tracing::debug!(
                            "vsock connect proxy session ended for {}",
                            unix_path.display()
                        );
                    });
                }
                Err(e) => {
                    tracing::warn!("unix socket accept error: {e}");
                }
            }
        }
    });

    Ok(())
}
