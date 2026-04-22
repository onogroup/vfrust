use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::Duration;

use vfrust::config::bootloader::{Bootloader, EfiBootloader, LinuxBootloader};
use vfrust::config::device::network::{MacAddress, NetAttachment, VirtioNet};
use vfrust::config::device::serial::{SerialAttachment, VirtioSerial};
use vfrust::config::device::storage::VirtioBlk;
use vfrust::config::device::Device;
use vfrust::{VmConfig, VmState};

// ---------------------------------------------------------------------------
// RAII guard for test files — ensures cleanup even on panic
// ---------------------------------------------------------------------------

/// Owns a file path and removes it on drop (even if the test panics).
pub struct TestFile(pub PathBuf);

impl TestFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl std::ops::Deref for TestFile {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestFile {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Tool lookup
// ---------------------------------------------------------------------------

fn find_in_nix_store(pkg_name: &str, bin_name: &str) -> Option<String> {
    for entry in std::fs::read_dir("/nix/store").ok()?.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.contains(pkg_name) && !name_str.ends_with(".drv") {
            let bin = entry.path().join("bin").join(bin_name);
            if bin.exists() {
                return Some(bin.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn find_tool(names: &[&str], nix_pkg: &str) -> String {
    for name in names {
        if Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return name.to_string();
        }
    }
    if let Some(path) = find_in_nix_store(nix_pkg, names[0]) {
        return path;
    }
    panic!("{} not found in PATH or /nix/store", names.join(" / "));
}

fn find_qemu_img() -> String {
    find_tool(&["qemu-img"], "qemu")
}

fn find_mkisofs() -> String {
    find_tool(&["mkisofs", "genisoimage"], "cdrtools")
}

// ---------------------------------------------------------------------------
// Test asset directory
// ---------------------------------------------------------------------------

pub fn test_assets_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-assets");
    std::fs::create_dir_all(&dir).expect("create test-assets dir");
    dir
}

// ---------------------------------------------------------------------------
// Base image (shared, created once via Once)
// ---------------------------------------------------------------------------

static UBUNTU_IMAGE_INIT: Once = Once::new();

/// Ensure the Ubuntu base image exists. Thread-safe via `Once`.
pub fn ensure_ubuntu_image() -> PathBuf {
    let assets = test_assets_dir();
    let qcow2_path = assets.join("ubuntu.qcow2");
    let raw_path = assets.join("ubuntu.raw");

    UBUNTU_IMAGE_INIT.call_once(|| {
        if raw_path.exists() {
            return;
        }

        // Download qcow2 if not cached
        if !qcow2_path.exists() {
            let cached =
                PathBuf::from("/tmp/vfrust-test/ubuntu-24.04-server-cloudimg-arm64.img");
            if cached.exists() {
                std::fs::copy(&cached, &qcow2_path).expect("copy cached qcow2");
            } else {
                eprintln!("Downloading Ubuntu 24.04 cloud image...");
                let status = Command::new("curl")
                    .args([
                        "-L", "-f", "-o",
                        qcow2_path.to_str().unwrap(),
                        "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-arm64.img",
                    ])
                    .status()
                    .expect("curl");
                assert!(status.success(), "failed to download Ubuntu image");
            }
        }

        eprintln!("Converting qcow2 to raw...");
        let qemu_img = find_qemu_img();
        let status = Command::new(&qemu_img)
            .args([
                "convert", "-f", "qcow2", "-O", "raw",
                qcow2_path.to_str().unwrap(),
                raw_path.to_str().unwrap(),
            ])
            .status()
            .expect("qemu-img convert");
        assert!(status.success(), "failed to convert image");

        let status = Command::new(&qemu_img)
            .args(["resize", "-f", "raw", raw_path.to_str().unwrap(), "10G"])
            .status()
            .expect("qemu-img resize");
        assert!(status.success(), "failed to resize image");
    });

    raw_path
}

static RAW_KERNEL_INIT: Once = Once::new();

/// Ensure a raw ARM64 Image kernel is available. Thread-safe via `Once`.
pub fn ensure_raw_kernel() -> PathBuf {
    let assets = test_assets_dir();
    let image = assets.join("Image");

    RAW_KERNEL_INIT.call_once(|| {
        if image.exists() {
            return;
        }

        // Try nix binary cache
        if let Ok(out) = Command::new("nix")
            .args([
                "build",
                "--system",
                "aarch64-linux",
                "nixpkgs#linuxPackages_latest.kernel",
                "--no-link",
                "--print-out-paths",
            ])
            .output()
        {
            if out.status.success() {
                let store_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let src = PathBuf::from(&store_path).join("Image");
                if src.exists() {
                    let _ = std::fs::copy(&src, &image);
                    return;
                }
            }
        }

        // Scan nix store for any existing kernel
        if let Some(found) = find_raw_kernel_in_nix_store() {
            let _ = std::fs::copy(&found, &image);
        }
    });

    image
}

fn find_raw_kernel_in_nix_store() -> Option<PathBuf> {
    for entry in std::fs::read_dir("/nix/store").ok()?.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.contains("linux-") && !s.ends_with(".drv") {
            let candidate = entry.path().join("Image");
            if candidate.exists() {
                if let Ok(buf) = std::fs::read(&candidate) {
                    if buf.len() >= 0x3c && &buf[0x38..0x3c] == b"ARMd" {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

/// Ensure Alpine initramfs is available.
pub fn ensure_alpine_initramfs() -> PathBuf {
    let assets = test_assets_dir();
    let initramfs = assets.join("initramfs");
    if initramfs.exists() {
        return initramfs;
    }

    let cached = PathBuf::from("/tmp/vfrust-test/initramfs");
    if cached.exists() {
        std::fs::copy(&cached, &initramfs).expect("copy cached initramfs");
        return initramfs;
    }

    eprintln!("Downloading Alpine Linux initramfs...");
    let status = Command::new("curl")
        .args([
            "-L",
            "-f",
            "-o",
            initramfs.to_str().unwrap(),
            "https://dl-cdn.alpinelinux.org/alpine/v3.21/releases/aarch64/netboot/initramfs-virt",
        ])
        .status()
        .expect("curl");
    assert!(status.success(), "failed to download initramfs");
    initramfs
}

// ---------------------------------------------------------------------------
// Per-test disk + ISO creation (unique names via test_name)
// ---------------------------------------------------------------------------

/// Create a fresh copy of the Ubuntu disk image for a test.
/// Returns a `TestFile` that removes the copy on drop.
pub fn create_test_disk(test_name: &str) -> TestFile {
    let src = ensure_ubuntu_image();
    let dst = test_assets_dir().join(format!("{test_name}.raw"));
    std::fs::copy(&src, &dst).expect("copy disk image for test");
    TestFile(dst)
}

/// Generate a cloud-init ISO with SSH key and unique hostname.
/// Returns a `TestFile` that removes the ISO on drop.
pub fn create_cloudinit_iso(test_name: &str) -> TestFile {
    let dir = tempfile::tempdir().expect("create tempdir");

    let ssh_key_path = dirs::home_dir()
        .expect("home dir")
        .join(".ssh/id_ed25519.pub");
    assert!(
        ssh_key_path.exists(),
        "SSH public key not found at {}. Generate with: ssh-keygen -t ed25519",
        ssh_key_path.display()
    );
    let ssh_pub = std::fs::read_to_string(&ssh_key_path).expect("read SSH public key");

    let user_data = format!(
        "#cloud-config\nusers:\n  - name: ubuntu\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    shell: /bin/bash\n    ssh_authorized_keys:\n      - {ssh_pub}\nssh_pwauth: false\npackage_update: false\n"
    );
    let meta_data = format!("instance-id: {test_name}\nlocal-hostname: {test_name}\n");
    let network_config = "version: 2\nethernets:\n  enp0s1:\n    dhcp4: true\n";

    std::fs::write(dir.path().join("user-data"), user_data).unwrap();
    std::fs::write(dir.path().join("meta-data"), meta_data).unwrap();
    std::fs::write(dir.path().join("network-config"), network_config).unwrap();

    let iso_path = test_assets_dir().join(format!("{test_name}-cloudinit.iso"));
    let _ = std::fs::remove_file(&iso_path);

    let mkisofs = find_mkisofs();
    let status = Command::new(&mkisofs)
        .args([
            "-output",
            iso_path.to_str().unwrap(),
            "-volid",
            "cidata",
            "-joliet",
            "-rock",
            dir.path().to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("mkisofs");
    assert!(status.success(), "failed to create cloud-init ISO");

    TestFile(iso_path)
}

// ---------------------------------------------------------------------------
// Unique MAC address generation (avoids DHCP lease conflicts between tests)
// ---------------------------------------------------------------------------

/// Fixed locally-administered MAC address for tests.
///
/// Uses a constant MAC so that the host ARP cache entry for the VM's IP
/// (assigned via DHCP) stays valid across test runs. Virtualization.framework
/// NAT always assigns the same IP to the same DHCP identifier, so using a
/// consistent MAC prevents stale ARP entries from blocking SSH connectivity
/// (macOS ARP timeout is 20 minutes).
///
/// This is safe because E2E tests run with `--test-threads=1`.
fn test_mac() -> MacAddress {
    MacAddress([0x02, 0x00, 0xDE, 0xAD, 0xBE, 0xEF])
}

// ---------------------------------------------------------------------------
// VM config builders
// ---------------------------------------------------------------------------

/// Build a standard EFI VM config for testing.
pub fn efi_vm_config(
    disk: &Path,
    cloudinit_iso: Option<&Path>,
    serial_log: Option<&Path>,
    extra_devices: Vec<Device>,
) -> VmConfig {
    efi_vm_config_with_mac(disk, cloudinit_iso, serial_log, extra_devices, test_mac())
}

/// Like [`efi_vm_config`] but with an explicit MAC address. Use when a
/// single test needs to run multiple VMs concurrently and must avoid
/// DHCP collisions on the same MAC.
pub fn efi_vm_config_with_mac(
    disk: &Path,
    cloudinit_iso: Option<&Path>,
    serial_log: Option<&Path>,
    extra_devices: Vec<Device>,
    mac: MacAddress,
) -> VmConfig {
    let efi_store = test_assets_dir().join(format!(
        "efi-{}.fd",
        disk.file_stem().unwrap().to_str().unwrap()
    ));
    let _ = std::fs::remove_file(&efi_store);

    let mut builder = VmConfig::builder()
        .cpus(2)
        .memory_mib(2048)
        .bootloader(Bootloader::Efi(EfiBootloader {
            variable_store_path: efi_store,
            create_variable_store: true,
        }))
        .device(Device::VirtioBlk(VirtioBlk {
            path: disk.to_path_buf(),
            ..Default::default()
        }))
        .device(Device::VirtioNet(VirtioNet {
            attachment: NetAttachment::Nat,
            mac_address: Some(mac),
        }))
        .device(Device::VirtioRng);

    if let Some(serial_path) = serial_log {
        builder = builder.device(Device::VirtioSerial(VirtioSerial {
            attachment: SerialAttachment::File {
                path: serial_path.to_path_buf(),
            },
        }));
    }

    if let Some(iso) = cloudinit_iso {
        builder = builder.device(Device::VirtioBlk(VirtioBlk {
            path: iso.to_path_buf(),
            read_only: true,
            ..Default::default()
        }));
    }

    for dev in extra_devices {
        builder = builder.device(dev);
    }

    builder.build().expect("build VM config")
}

/// Build a Linux direct-boot VM config.
pub fn linux_vm_config(kernel: &Path, initrd: &Path, serial_log: &Path) -> VmConfig {
    VmConfig::builder()
        .cpus(1)
        .memory_mib(512)
        .bootloader(Bootloader::Linux(LinuxBootloader {
            kernel_path: kernel.to_path_buf(),
            initrd_path: Some(initrd.to_path_buf()),
            command_line: "console=hvc0".to_string(),
        }))
        .device(Device::VirtioSerial(VirtioSerial {
            attachment: SerialAttachment::File {
                path: serial_log.to_path_buf(),
            },
        }))
        .device(Device::VirtioRng)
        .build()
        .expect("build Linux VM config")
}

// ---------------------------------------------------------------------------
// VM interaction helpers
// ---------------------------------------------------------------------------

/// Format a `MacAddress` the way macOS `arp -an` displays it
/// (lowercase, no leading zeros, colon-separated).
fn mac_to_arp_format(mac: &MacAddress) -> String {
    mac.0
        .iter()
        .map(|b| format!("{b:x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Look up an IP address in the host ARP table by MAC address.
///
/// Parses `arp -an` output for a line matching the given MAC on a bridge
/// interface and extracts the IP.
fn resolve_ip_from_arp(mac_str: &str) -> Option<String> {
    let output = Command::new("arp").arg("-an").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // Format: ? (192.168.64.2) at 2:0:de:ad:be:ef on bridge100 ...
        if line.contains(mac_str) && line.contains("bridge") {
            let start = line.find('(')? + 1;
            let end = line.find(')')?;
            return Some(line[start..end].to_string());
        }
    }
    None
}

/// Find the VM's IP address by MAC + hostname verification.
///
/// 1. Polls the host ARP table for the test MAC (instant, no SSH).
/// 2. Once the IP is known, verifies SSH connectivity and hostname.
pub async fn find_vm_ip(expected_hostname: &str, timeout: Duration) -> Option<String> {
    find_vm_ip_with_mac(expected_hostname, test_mac(), timeout).await
}

/// Like [`find_vm_ip`] but with an explicit MAC. Use when a test builds its
/// VM with a non-default MAC via [`efi_vm_config_with_mac`] (e.g. to avoid
/// ARP-cache aliasing across back-to-back single-VM tests that reuse the
/// default test MAC).
pub async fn find_vm_ip_with_mac(
    expected_hostname: &str,
    mac: MacAddress,
    timeout: Duration,
) -> Option<String> {
    let hostname = expected_hostname.to_string();
    let mac_str = mac_to_arp_format(&mac);

    tokio::task::spawn_blocking(move || {
        let start = std::time::Instant::now();
        let key_path = dirs::home_dir().unwrap().join(".ssh/id_ed25519");

        // Phase 1: resolve IP from ARP table (sub-second once VM has DHCP)
        let ip = loop {
            if start.elapsed() >= timeout {
                return None;
            }
            if let Some(ip) = resolve_ip_from_arp(&mac_str) {
                break ip;
            }
            std::thread::sleep(Duration::from_millis(500));
        };

        // Phase 2: wait for SSH + correct hostname on the known IP
        while start.elapsed() < timeout {
            let output = Command::new("ssh")
                .args([
                    "-o",
                    "StrictHostKeyChecking=no",
                    "-o",
                    "UserKnownHostsFile=/dev/null",
                    "-o",
                    "ConnectTimeout=2",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "LogLevel=ERROR",
                    "-i",
                    &key_path.to_string_lossy(),
                    &format!("ubuntu@{ip}"),
                    "hostname",
                ])
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    let actual = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if actual == hostname {
                        return Some(ip);
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// SSH into a VM and run a command, returning stdout.
pub fn ssh_command(ip: &str, cmd: &str) -> Result<String, String> {
    let key_path = dirs::home_dir().unwrap().join(".ssh/id_ed25519");
    let output = Command::new("ssh")
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "BatchMode=yes",
            "-o",
            "LogLevel=ERROR",
            "-i",
            &key_path.to_string_lossy(),
            &format!("ubuntu@{ip}"),
            cmd,
        ])
        .output()
        .map_err(|e| format!("ssh exec failed: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "ssh exit {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// SSH into a VM retrying until success or timeout.
/// Each attempt's wait is bounded by SSH's ConnectTimeout (5s).
pub fn ssh_retry(ip: &str, cmd: &str, timeout: Duration) -> Result<String, String> {
    let start = std::time::Instant::now();
    let mut last_err = String::new();
    while start.elapsed() < timeout {
        match ssh_command(ip, cmd) {
            Ok(output) => return Ok(output),
            Err(e) => last_err = e,
        }
    }
    Err(format!("ssh_retry timed out after {timeout:?}: {last_err}"))
}

/// Stop a VM and wait for it to reach Stopped state.
pub async fn stop_and_wait(handle: &vfrust::VmHandle) {
    // Try force stop regardless of current state
    let _ = handle.stop().await;
    let mut rx = handle.state_stream();
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if *rx.borrow() == VmState::Stopped {
                break;
            }
            rx.changed().await.ok();
        }
    })
    .await;
}

/// RAII guard that stops a VM on drop. Use in tests to ensure cleanup.
pub struct VmGuard {
    handle: vfrust::VmHandle,
    rt: tokio::runtime::Handle,
}

impl VmGuard {
    pub fn new(handle: &vfrust::VmHandle) -> Self {
        Self {
            handle: handle.clone(),
            rt: tokio::runtime::Handle::current(),
        }
    }
}

impl Drop for VmGuard {
    fn drop(&mut self) {
        let handle = self.handle.clone();
        // Spawn the stop on the runtime and detach — we can't block_on inside tokio.
        // The dispatch queue will process the stop asynchronously.
        // Use spawn_blocking to avoid nested runtime issues.
        let _ = std::thread::spawn(move || {
            // Create a fresh runtime for cleanup since we may be inside a panicking tokio task
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                let _ = rt.block_on(async {
                    let _ = handle.stop().await;
                    let mut rx = handle.state_stream();
                    let _ = tokio::time::timeout(Duration::from_secs(10), async {
                        loop {
                            if *rx.borrow() == VmState::Stopped {
                                break;
                            }
                            rx.changed().await.ok();
                        }
                    })
                    .await;
                });
            }
        })
        .join();
    }
}

/// Wait for a specific string to appear in a file (with timeout).
/// Polls the file at ~100ms intervals.
pub async fn wait_for_file_content(path: &Path, needle: &str, timeout: Duration) -> bool {
    let path = path.to_path_buf();
    let needle = needle.to_string();
    tokio::task::spawn_blocking(move || {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains(&needle) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    })
    .await
    .unwrap_or(false)
}
