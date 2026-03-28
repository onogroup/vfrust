use std::ffi::CString;

use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_virtualization::*;

use crate::config::device::audio::VirtioSound;
use crate::config::device::fs::{Rosetta, VirtioFs};
use crate::config::device::gpu::{MacGraphics, VirtioGpu};
use crate::config::device::input::VirtioInput;
use crate::config::device::network::{NetAttachment, VirtioNet};
use crate::config::device::serial::{SerialAttachment, VirtioSerial};
use crate::config::device::storage::{
    DiskBackend, DiskCachingMode, DiskSyncMode, Nbd, Nvme, UsbMassStorage, VirtioBlk,
};
use crate::config::device::vsock::VirtioVsock;
use crate::config::device::Device;
use crate::sys::nsurl_from_path;

pub(crate) struct BuiltDevices {
    pub storage: Retained<NSArray<VZStorageDeviceConfiguration>>,
    pub network: Retained<NSArray<VZNetworkDeviceConfiguration>>,
    pub serial: Retained<NSArray<VZSerialPortConfiguration>>,
    pub socket: Retained<NSArray<VZSocketDeviceConfiguration>>,
    pub entropy: Retained<NSArray<VZEntropyDeviceConfiguration>>,
    pub balloon: Retained<NSArray<VZMemoryBalloonDeviceConfiguration>>,
    pub graphics: Retained<NSArray<VZGraphicsDeviceConfiguration>>,
    pub keyboard: Retained<NSArray<VZKeyboardConfiguration>>,
    pub pointing: Retained<NSArray<VZPointingDeviceConfiguration>>,
    pub dir_sharing: Retained<NSArray<VZDirectorySharingDeviceConfiguration>>,
    pub audio: Retained<NSArray<VZAudioDeviceConfiguration>>,
    pub usb_controllers: Retained<NSArray<VZUSBControllerConfiguration>>,
}

pub(crate) fn build_devices(devices: &[Device]) -> crate::Result<BuiltDevices> {
    let mut storage_list: Vec<Retained<VZStorageDeviceConfiguration>> = Vec::new();
    let mut network_list: Vec<Retained<VZNetworkDeviceConfiguration>> = Vec::new();
    let mut serial_list: Vec<Retained<VZSerialPortConfiguration>> = Vec::new();
    let mut socket_list: Vec<Retained<VZSocketDeviceConfiguration>> = Vec::new();
    let mut entropy_list: Vec<Retained<VZEntropyDeviceConfiguration>> = Vec::new();
    let mut balloon_list: Vec<Retained<VZMemoryBalloonDeviceConfiguration>> = Vec::new();
    let mut graphics_list: Vec<Retained<VZGraphicsDeviceConfiguration>> = Vec::new();
    let mut keyboard_list: Vec<Retained<VZKeyboardConfiguration>> = Vec::new();
    let mut pointing_list: Vec<Retained<VZPointingDeviceConfiguration>> = Vec::new();
    let mut dir_sharing_list: Vec<Retained<VZDirectorySharingDeviceConfiguration>> = Vec::new();
    let mut audio_list: Vec<Retained<VZAudioDeviceConfiguration>> = Vec::new();
    let mut usb_controller_list: Vec<Retained<VZUSBControllerConfiguration>> = Vec::new();

    for device in devices {
        match device {
            Device::VirtioBlk(blk) => storage_list.push(build_virtio_blk(blk)?),
            Device::Nvme(nvme) => storage_list.push(build_nvme(nvme)?),
            Device::UsbMassStorage(usb) => storage_list.push(build_usb_mass_storage(usb)?),
            Device::Nbd(nbd) => storage_list.push(build_nbd(nbd)?),
            Device::VirtioNet(net) => network_list.push(build_virtio_net(net)?),
            Device::VirtioSerial(serial) => serial_list.push(build_virtio_serial(serial)?),
            Device::VirtioVsock(vsock) => socket_list.push(build_virtio_vsock(vsock)?),
            Device::VirtioGpu(gpu) => graphics_list.push(build_virtio_gpu(gpu)?),
            Device::MacGraphics(mac_gpu) => graphics_list.push(build_mac_graphics(mac_gpu)?),
            Device::VirtioInput(input) => match input {
                VirtioInput::Keyboard => keyboard_list.push(build_keyboard()?),
                VirtioInput::Pointing => pointing_list.push(build_pointing()?),
            },
            Device::VirtioFs(fs) => dir_sharing_list.push(build_virtio_fs(fs)?),
            Device::Rosetta(rosetta) => dir_sharing_list.push(build_rosetta(rosetta)?),
            Device::VirtioSound(sound) => audio_list.push(build_virtio_sound(sound)?),
            Device::VirtioRng => entropy_list.push(build_virtio_rng()?),
            Device::VirtioBalloon => balloon_list.push(build_virtio_balloon()?),
            Device::UsbController => usb_controller_list.push(build_usb_controller()?),
        }
    }

    Ok(BuiltDevices {
        storage: NSArray::from_retained_slice(&storage_list),
        network: NSArray::from_retained_slice(&network_list),
        serial: NSArray::from_retained_slice(&serial_list),
        socket: NSArray::from_retained_slice(&socket_list),
        entropy: NSArray::from_retained_slice(&entropy_list),
        balloon: NSArray::from_retained_slice(&balloon_list),
        graphics: NSArray::from_retained_slice(&graphics_list),
        keyboard: NSArray::from_retained_slice(&keyboard_list),
        pointing: NSArray::from_retained_slice(&pointing_list),
        dir_sharing: NSArray::from_retained_slice(&dir_sharing_list),
        audio: NSArray::from_retained_slice(&audio_list),
        usb_controllers: NSArray::from_retained_slice(&usb_controller_list),
    })
}

// ---------------------------------------------------------------------------
// Block device identifier validation (for VirtioBlk and NBD)
// ---------------------------------------------------------------------------

fn validate_block_device_id(id: &str) -> crate::Result<()> {
    if !id.is_ascii() || id.len() > 20 {
        return Err(crate::Error::InvalidDevice(format!(
            "block device identifier must be at most 20 ASCII bytes, got {} bytes",
            id.len()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Storage devices
// ---------------------------------------------------------------------------

fn build_virtio_blk(blk: &VirtioBlk) -> crate::Result<Retained<VZStorageDeviceConfiguration>> {
    unsafe {
        let attachment: Retained<VZStorageDeviceAttachment> = match blk.backend {
            DiskBackend::Image => {
                let url = nsurl_from_path(&blk.path)?;
                let caching = match blk.caching_mode {
                    DiskCachingMode::Automatic => VZDiskImageCachingMode::Automatic,
                    DiskCachingMode::Cached => VZDiskImageCachingMode::Cached,
                    DiskCachingMode::Uncached => VZDiskImageCachingMode::Uncached,
                };
                let sync = match blk.sync_mode {
                    DiskSyncMode::Full => VZDiskImageSynchronizationMode::Full,
                    DiskSyncMode::Fsync => VZDiskImageSynchronizationMode::Fsync,
                    DiskSyncMode::None => VZDiskImageSynchronizationMode::None,
                };

                let img_attachment =
                    VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
                        VZDiskImageStorageDeviceAttachment::alloc(),
                        &url,
                        blk.read_only,
                        caching,
                        sync,
                    )
                    .map_err(|e| crate::Error::InvalidDevice(format!("virtio-blk attachment: {e}")))?;
                Retained::into_super(img_attachment)
            }
            DiskBackend::BlockDevice => {
                let path_str = blk.path.to_str().ok_or_else(|| {
                    crate::Error::InvalidDevice("block device path is not valid UTF-8".into())
                })?;
                let c_path = CString::new(path_str).map_err(|_| {
                    crate::Error::InvalidDevice("block device path contains null bytes".into())
                })?;

                // Verify it is actually a block device via stat
                let mut stat_buf: libc::stat = std::mem::zeroed();
                if libc::stat(c_path.as_ptr(), &mut stat_buf) != 0 {
                    return Err(crate::Error::InvalidDevice(format!(
                        "cannot stat block device path: {}",
                        std::io::Error::last_os_error()
                    )));
                }
                if (stat_buf.st_mode & libc::S_IFMT) != libc::S_IFBLK {
                    return Err(crate::Error::InvalidDevice(format!(
                        "path is not a block device: {}",
                        path_str
                    )));
                }

                let open_flags = if blk.read_only {
                    libc::O_RDONLY
                } else {
                    libc::O_RDWR
                };
                let fd = libc::open(c_path.as_ptr(), open_flags);
                if fd < 0 {
                    return Err(crate::Error::InvalidDevice(format!(
                        "cannot open block device: {}",
                        std::io::Error::last_os_error()
                    )));
                }

                let file_handle = objc2_foundation::NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                    objc2_foundation::NSFileHandle::alloc(),
                    fd,
                    true, // NSFileHandle takes ownership, will close on dealloc
                );

                let blk_attachment =
                    VZDiskBlockDeviceStorageDeviceAttachment::initWithFileHandle_readOnly_synchronizationMode_error(
                        VZDiskBlockDeviceStorageDeviceAttachment::alloc(),
                        &file_handle,
                        blk.read_only,
                        VZDiskSynchronizationMode::Full,
                    )
                    .map_err(|e| {
                        crate::Error::InvalidDevice(format!("block device attachment: {e}"))
                    })?;
                Retained::into_super(blk_attachment)
            }
        };

        let config = VZVirtioBlockDeviceConfiguration::initWithAttachment(
            VZVirtioBlockDeviceConfiguration::alloc(),
            &attachment,
        );

        if let Some(id) = &blk.device_id {
            validate_block_device_id(id)?;
            let ns_id = NSString::from_str(id);
            config.setBlockDeviceIdentifier(&ns_id);
        }

        Ok(Retained::into_super(config))
    }
}

fn build_nvme(nvme: &Nvme) -> crate::Result<Retained<VZStorageDeviceConfiguration>> {
    let url = nsurl_from_path(&nvme.path)?;
    unsafe {
        let attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &url,
            nvme.read_only,
        )
        .map_err(|e| crate::Error::InvalidDevice(format!("nvme attachment: {e}")))?;

        let config = VZNVMExpressControllerDeviceConfiguration::initWithAttachment(
            VZNVMExpressControllerDeviceConfiguration::alloc(),
            &attachment,
        );

        Ok(Retained::into_super(config))
    }
}

fn build_usb_mass_storage(
    usb: &UsbMassStorage,
) -> crate::Result<Retained<VZStorageDeviceConfiguration>> {
    let url = nsurl_from_path(&usb.path)?;
    unsafe {
        let attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &url,
            usb.read_only,
        )
        .map_err(|e| crate::Error::InvalidDevice(format!("usb-mass-storage attachment: {e}")))?;

        let config = VZUSBMassStorageDeviceConfiguration::initWithAttachment(
            VZUSBMassStorageDeviceConfiguration::alloc(),
            &attachment,
        );

        Ok(Retained::into_super(config))
    }
}

fn build_nbd(nbd: &Nbd) -> crate::Result<Retained<VZStorageDeviceConfiguration>> {
    let ns_uri_string = NSString::from_str(&nbd.uri);
    unsafe {
        let url = objc2_foundation::NSURL::URLWithString(&ns_uri_string)
            .ok_or_else(|| crate::Error::InvalidDevice(format!("invalid NBD URI: {}", nbd.uri)))?;

        let sync = match nbd.sync_mode {
            DiskSyncMode::Full => VZDiskSynchronizationMode::Full,
            DiskSyncMode::Fsync => VZDiskSynchronizationMode::Full, // NBD only supports Full/None
            DiskSyncMode::None => VZDiskSynchronizationMode::None,
        };

        let attachment =
            VZNetworkBlockDeviceStorageDeviceAttachment::initWithURL_timeout_forcedReadOnly_synchronizationMode_error(
                VZNetworkBlockDeviceStorageDeviceAttachment::alloc(),
                &url,
                nbd.timeout.map(|d| d.as_secs_f64()).unwrap_or(0.0),
                nbd.read_only,
                sync,
            )
            .map_err(|e| crate::Error::InvalidDevice(format!("nbd attachment: {e}")))?;

        let config = VZVirtioBlockDeviceConfiguration::initWithAttachment(
            VZVirtioBlockDeviceConfiguration::alloc(),
            &attachment,
        );

        if let Some(id) = &nbd.device_id {
            validate_block_device_id(id)?;
            let ns_id = NSString::from_str(id);
            config.setBlockDeviceIdentifier(&ns_id);
        }

        Ok(Retained::into_super(config))
    }
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

fn build_virtio_net(net: &VirtioNet) -> crate::Result<Retained<VZNetworkDeviceConfiguration>> {
    unsafe {
        let config = VZVirtioNetworkDeviceConfiguration::new();

        match &net.attachment {
            NetAttachment::Nat => {
                let attachment = VZNATNetworkDeviceAttachment::new();
                config.setAttachment(Some(&attachment));
            }
            NetAttachment::FileDescriptor { fd } => {
                let fh = objc2_foundation::NSFileHandle::initWithFileDescriptor(
                    objc2_foundation::NSFileHandle::alloc(),
                    *fd,
                );
                let attachment = VZFileHandleNetworkDeviceAttachment::initWithFileHandle(
                    VZFileHandleNetworkDeviceAttachment::alloc(),
                    &fh,
                );
                config.setAttachment(Some(&attachment));
            }
            NetAttachment::UnixSocket { path } => {
                let fh = create_unix_socket_filehandle(path)?;
                let attachment = VZFileHandleNetworkDeviceAttachment::initWithFileHandle(
                    VZFileHandleNetworkDeviceAttachment::alloc(),
                    &fh,
                );
                config.setAttachment(Some(&attachment));
            }
        }

        if let Some(mac) = &net.mac_address {
            let mac_str = mac.to_string();
            let ns_mac_str = NSString::from_str(&mac_str);
            if let Some(vz_mac) =
                VZMACAddress::initWithString(VZMACAddress::alloc(), &ns_mac_str)
            {
                config.setMACAddress(&vz_mac);
            }
        }

        Ok(Retained::into_super(config))
    }
}

/// Create a connected unixgram socket and wrap it in an NSFileHandle.
fn create_unix_socket_filehandle(
    remote_path: &std::path::Path,
) -> crate::Result<Retained<objc2_foundation::NSFileHandle>> {
    let remote_str = remote_path.to_str().ok_or_else(|| {
        crate::Error::InvalidDevice("unix socket path is not valid UTF-8".into())
    })?;

    // macOS sun_path is 104 bytes
    const SUN_PATH_MAX: usize = 104;
    if remote_str.as_bytes().len() >= SUN_PATH_MAX {
        return Err(crate::Error::InvalidDevice(format!(
            "unix socket path too long ({} bytes, max {})",
            remote_str.as_bytes().len(),
            SUN_PATH_MAX - 1
        )));
    }

    unsafe {
        // Create a SOCK_DGRAM unix socket
        let sock = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0);
        if sock < 0 {
            return Err(crate::Error::InvalidDevice(format!(
                "socket(AF_UNIX, SOCK_DGRAM) failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        // Bind to an ephemeral (autobind) local address
        // On macOS, we need to create a temp path and bind to it
        let tmp_dir = std::env::temp_dir();
        let local_path = tmp_dir.join(format!("vfrust-net-{}", libc::getpid()));
        let local_str = local_path.to_str().unwrap_or("/tmp/vfrust-net-ephemeral");

        // Remove stale socket if present
        let local_c = CString::new(local_str).map_err(|_| {
            libc::close(sock);
            crate::Error::InvalidDevice("local socket path contains null bytes".into())
        })?;
        libc::unlink(local_c.as_ptr());

        let mut local_addr: libc::sockaddr_un = std::mem::zeroed();
        local_addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let local_bytes = local_str.as_bytes();
        if local_bytes.len() >= SUN_PATH_MAX {
            libc::close(sock);
            return Err(crate::Error::InvalidDevice(
                "local ephemeral socket path too long".into(),
            ));
        }
        std::ptr::copy_nonoverlapping(
            local_bytes.as_ptr(),
            local_addr.sun_path.as_mut_ptr() as *mut u8,
            local_bytes.len(),
        );

        let bind_ret = libc::bind(
            sock,
            &local_addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        );
        if bind_ret != 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(crate::Error::InvalidDevice(format!(
                "bind() failed for unix socket: {err}"
            )));
        }

        // Connect to remote path
        let _remote_c = CString::new(remote_str).map_err(|_| {
            libc::close(sock);
            crate::Error::InvalidDevice("remote socket path contains null bytes".into())
        })?;
        let mut remote_addr: libc::sockaddr_un = std::mem::zeroed();
        remote_addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let remote_bytes = remote_str.as_bytes();
        std::ptr::copy_nonoverlapping(
            remote_bytes.as_ptr(),
            remote_addr.sun_path.as_mut_ptr() as *mut u8,
            remote_bytes.len(),
        );

        let conn_ret = libc::connect(
            sock,
            &remote_addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        );
        if conn_ret != 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(crate::Error::InvalidDevice(format!(
                "connect() failed for unix socket to {remote_str}: {err}"
            )));
        }

        // Set socket buffer sizes: SO_SNDBUF=1MB, SO_RCVBUF=4MB
        let sndbuf: libc::c_int = 1024 * 1024;
        let rcvbuf: libc::c_int = 4 * 1024 * 1024;
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &sndbuf as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &rcvbuf as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );

        // Dup the FD so NSFileHandle can own its own copy
        let dup_fd = libc::dup(sock);
        if dup_fd < 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(crate::Error::InvalidDevice(format!(
                "dup() failed: {err}"
            )));
        }
        libc::close(sock);

        let fh = objc2_foundation::NSFileHandle::initWithFileDescriptor_closeOnDealloc(
            objc2_foundation::NSFileHandle::alloc(),
            dup_fd,
            true,
        );

        Ok(fh)
    }
}

// ---------------------------------------------------------------------------
// Serial
// ---------------------------------------------------------------------------

/// Set raw mode on a file descriptor: clear ICRNL from iflag, clear ICANON|ECHO from lflag.
fn set_raw_mode(fd: libc::c_int) {
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut termios) == 0 {
            termios.c_iflag &= !libc::ICRNL;
            termios.c_lflag &= !(libc::ICANON | libc::ECHO);
            libc::tcsetattr(fd, libc::TCSANOW, &termios);
        }
    }
}

fn build_virtio_serial(
    serial: &VirtioSerial,
) -> crate::Result<Retained<VZSerialPortConfiguration>> {
    unsafe {
        let attachment: Retained<VZSerialPortAttachment> = match &serial.attachment {
            SerialAttachment::File { path } => {
                let url = nsurl_from_path(path)?;
                let attachment = VZFileSerialPortAttachment::initWithURL_append_error(
                    VZFileSerialPortAttachment::alloc(),
                    &url,
                    true,
                )
                .map_err(|e| {
                    crate::Error::InvalidDevice(format!("serial file attachment: {e}"))
                })?;
                Retained::into_super(attachment)
            }
            SerialAttachment::Stdio => {
                // Set raw mode on stdin for interactive console
                set_raw_mode(libc::STDIN_FILENO);
                let stdin = objc2_foundation::NSFileHandle::fileHandleWithStandardInput();
                let stdout = objc2_foundation::NSFileHandle::fileHandleWithStandardOutput();
                let attachment =
                    VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                        VZFileHandleSerialPortAttachment::alloc(),
                        Some(&stdin),
                        Some(&stdout),
                    );
                Retained::into_super(attachment)
            }
            // TODO: vfkit uses VZVirtioConsolePortConfiguration (added to consoleDevices)
            // for PTY attachments instead of VZVirtioConsoleDeviceSerialPortConfiguration
            // (serialPorts). The objc2-virtualization crate exposes the needed types behind
            // the VZConsolePortConfiguration and VZVirtioConsolePortConfiguration features,
            // but wiring them requires adding a `console` field to BuiltDevices,
            // setConsoleDevices() in config_builder, and splitting PTY vs Stdio/File into
            // different device arrays. Keeping serial port path for now for simplicity.
            SerialAttachment::Pty => {
                let (master_fh, _slave_path) = open_pty_raw()?;
                let attachment =
                    VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                        VZFileHandleSerialPortAttachment::alloc(),
                        Some(&master_fh),
                        Some(&master_fh),
                    );
                Retained::into_super(attachment)
            }
        };

        let config = VZVirtioConsoleDeviceSerialPortConfiguration::new();
        config.setAttachment(Some(&attachment));

        Ok(Retained::into_super(config))
    }
}

/// Open a PTY pair, configure raw mode on the master, and return (master NSFileHandle, slave path).
fn open_pty_raw() -> crate::Result<(Retained<objc2_foundation::NSFileHandle>, String)> {
    unsafe {
        let mut master_fd: libc::c_int = -1;
        let mut slave_fd: libc::c_int = -1;

        let ret = libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if ret != 0 {
            return Err(crate::Error::InvalidDevice(format!(
                "openpty() failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        // Set raw mode on master (matching vfkit behavior): clear ICRNL from iflag, clear ICANON|ECHO from lflag
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(master_fd, &mut termios) == 0 {
            termios.c_iflag &= !libc::ICRNL;
            termios.c_lflag &= !(libc::ICANON | libc::ECHO);
            libc::tcsetattr(master_fd, libc::TCSANOW, &termios);
        }

        // Get the slave device name
        let slave_name_ptr = libc::ptsname(master_fd);
        let slave_path = if !slave_name_ptr.is_null() {
            std::ffi::CStr::from_ptr(slave_name_ptr)
                .to_string_lossy()
                .into_owned()
        } else {
            // Fallback: use ttyname on the slave fd
            let tty_ptr = libc::ttyname(slave_fd);
            if !tty_ptr.is_null() {
                std::ffi::CStr::from_ptr(tty_ptr)
                    .to_string_lossy()
                    .into_owned()
            } else {
                "<unknown>".to_string()
            }
        };

        tracing::info!("PTY serial: slave device at {slave_path}");

        // Close the slave FD; the guest will open it via the VZ framework
        libc::close(slave_fd);

        // Wrap master FD in NSFileHandle (will close on dealloc)
        let master_fh = objc2_foundation::NSFileHandle::initWithFileDescriptor_closeOnDealloc(
            objc2_foundation::NSFileHandle::alloc(),
            master_fd,
            true,
        );

        Ok((master_fh, slave_path))
    }
}

// ---------------------------------------------------------------------------
// Vsock
// ---------------------------------------------------------------------------

fn build_virtio_vsock(
    _vsock: &VirtioVsock,
) -> crate::Result<Retained<VZSocketDeviceConfiguration>> {
    unsafe {
        let config = VZVirtioSocketDeviceConfiguration::new();
        Ok(Retained::into_super(config))
    }
}

// ---------------------------------------------------------------------------
// Graphics
// ---------------------------------------------------------------------------

fn build_virtio_gpu(gpu: &VirtioGpu) -> crate::Result<Retained<VZGraphicsDeviceConfiguration>> {
    unsafe {
        let scanout = VZVirtioGraphicsScanoutConfiguration::initWithWidthInPixels_heightInPixels(
            VZVirtioGraphicsScanoutConfiguration::alloc(),
            gpu.width as isize,
            gpu.height as isize,
        );
        let scanouts = NSArray::from_retained_slice(&[scanout]);

        let config = VZVirtioGraphicsDeviceConfiguration::new();
        config.setScanouts(&scanouts);

        Ok(Retained::into_super(config))
    }
}

fn build_mac_graphics(
    mac_gpu: &MacGraphics,
) -> crate::Result<Retained<VZGraphicsDeviceConfiguration>> {
    unsafe {
        let display = VZMacGraphicsDisplayConfiguration::initWithWidthInPixels_heightInPixels_pixelsPerInch(
            VZMacGraphicsDisplayConfiguration::alloc(),
            mac_gpu.width as isize,
            mac_gpu.height as isize,
            mac_gpu.pixels_per_inch as isize,
        );
        let displays = NSArray::from_retained_slice(&[display]);

        let config = VZMacGraphicsDeviceConfiguration::new();
        config.setDisplays(&displays);

        Ok(Retained::into_super(config))
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

fn build_keyboard() -> crate::Result<Retained<VZKeyboardConfiguration>> {
    unsafe {
        let config = VZUSBKeyboardConfiguration::new();
        Ok(Retained::into_super(config))
    }
}

fn build_pointing() -> crate::Result<Retained<VZPointingDeviceConfiguration>> {
    unsafe {
        let config = VZUSBScreenCoordinatePointingDeviceConfiguration::new();
        Ok(Retained::into_super(config))
    }
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

fn build_virtio_sound(
    sound: &VirtioSound,
) -> crate::Result<Retained<VZAudioDeviceConfiguration>> {
    unsafe {
        let config = VZVirtioSoundDeviceConfiguration::new();

        let mut streams: Vec<Retained<VZVirtioSoundDeviceStreamConfiguration>> = Vec::new();

        if sound.output {
            let output_stream = VZVirtioSoundDeviceOutputStreamConfiguration::new();
            let sink = VZHostAudioOutputStreamSink::new();
            output_stream.setSink(Some(&sink));
            streams.push(Retained::into_super(output_stream));
        }

        if sound.input {
            let input_stream = VZVirtioSoundDeviceInputStreamConfiguration::new();
            let source = VZHostAudioInputStreamSource::new();
            input_stream.setSource(Some(&source));
            streams.push(Retained::into_super(input_stream));
        }

        let ns_streams = NSArray::from_retained_slice(&streams);
        config.setStreams(&ns_streams);

        Ok(Retained::into_super(config))
    }
}

// ---------------------------------------------------------------------------
// USB Controller
// ---------------------------------------------------------------------------

fn build_usb_controller() -> crate::Result<Retained<VZUSBControllerConfiguration>> {
    unsafe {
        let config = VZXHCIControllerConfiguration::new();
        Ok(Retained::into_super(config))
    }
}

// ---------------------------------------------------------------------------
// Directory sharing / VirtioFs
// ---------------------------------------------------------------------------

fn build_virtio_fs(
    fs: &VirtioFs,
) -> crate::Result<Retained<VZDirectorySharingDeviceConfiguration>> {
    unsafe {
        let tag = NSString::from_str(&fs.mount_tag);
        let config = VZVirtioFileSystemDeviceConfiguration::initWithTag(
            VZVirtioFileSystemDeviceConfiguration::alloc(),
            &tag,
        );

        if !fs.directories.is_empty() {
            // Multiple directory share
            let mut keys: Vec<Retained<NSString>> = Vec::new();
            let mut values: Vec<Retained<VZSharedDirectory>> = Vec::new();
            for dir in &fs.directories {
                let dir_url = nsurl_from_path(&dir.path)?;
                let shared_dir = VZSharedDirectory::initWithURL_readOnly(
                    VZSharedDirectory::alloc(),
                    &dir_url,
                    dir.read_only,
                );
                keys.push(NSString::from_str(&dir.name));
                values.push(shared_dir);
            }

            let keys_ref: Vec<&NSString> = keys.iter().map(|k| k.as_ref()).collect();
            let values_ref: Vec<&VZSharedDirectory> = values.iter().map(|v| v.as_ref()).collect();
            let dict = NSDictionary::from_slices(&keys_ref, &values_ref);

            let share = VZMultipleDirectoryShare::initWithDirectories(
                VZMultipleDirectoryShare::alloc(),
                &dict,
            );
            config.setShare(Some(&share));
        } else if let Some(dir_path) = &fs.shared_dir {
            // Single directory share
            let dir_url = nsurl_from_path(dir_path)?;
            let shared_dir = VZSharedDirectory::initWithURL_readOnly(
                VZSharedDirectory::alloc(),
                &dir_url,
                false,
            );
            let share = VZSingleDirectoryShare::initWithDirectory(
                VZSingleDirectoryShare::alloc(),
                &shared_dir,
            );
            config.setShare(Some(&share));
        } else {
            return Err(crate::Error::InvalidDevice(
                "virtio-fs requires either shared_dir or directories to be set".into(),
            ));
        }

        Ok(Retained::into_super(config))
    }
}

// ---------------------------------------------------------------------------
// Rosetta
// ---------------------------------------------------------------------------

fn build_rosetta(
    rosetta: &Rosetta,
) -> crate::Result<Retained<VZDirectorySharingDeviceConfiguration>> {
    unsafe {
        let availability = VZLinuxRosettaDirectoryShare::availability();

        if availability == VZLinuxRosettaAvailability::NotSupported {
            return Err(crate::Error::RosettaUnavailable);
        }

        if availability == VZLinuxRosettaAvailability::NotInstalled {
            if rosetta.install {
                tracing::info!("rosetta not installed, initiating installation...");
                install_rosetta_sync()?;
                tracing::info!("rosetta installation completed");
            } else if rosetta.ignore_if_missing {
                return Err(crate::Error::RosettaUnavailable);
            } else {
                return Err(crate::Error::RosettaUnavailable);
            }
        }

        // Use initWithError instead of new() for proper error handling
        let share = VZLinuxRosettaDirectoryShare::initWithError(
            VZLinuxRosettaDirectoryShare::alloc(),
        )
        .map_err(|e| {
            crate::Error::InvalidDevice(format!("failed to create rosetta directory share: {e}"))
        })?;

        let tag = NSString::from_str(&rosetta.mount_tag);
        let config = VZVirtioFileSystemDeviceConfiguration::initWithTag(
            VZVirtioFileSystemDeviceConfiguration::alloc(),
            &tag,
        );
        config.setShare(Some(&share));

        Ok(Retained::into_super(config))
    }
}

/// Synchronously install Rosetta by blocking on the completion handler.
fn install_rosetta_sync() -> crate::Result<()> {
    use block2::RcBlock;
    use std::sync::Mutex;

    let (tx, rx) = std::sync::mpsc::channel::<crate::Result<()>>();
    let tx = Mutex::new(Some(tx));

    let block = RcBlock::new(move |err: *mut objc2_foundation::NSError| {
        if let Some(tx) = tx.lock().unwrap().take() {
            if err.is_null() {
                let _ = tx.send(Ok(()));
            } else {
                let error = unsafe { &*err };
                let _ = tx.send(Err(crate::Error::InvalidDevice(format!(
                    "rosetta installation failed: {error}"
                ))));
            }
        }
    });

    unsafe {
        VZLinuxRosettaDirectoryShare::installRosettaWithCompletionHandler(&block);
    }

    rx.recv()
        .map_err(|_| crate::Error::DispatchError("rosetta install channel closed".into()))?
}

// ---------------------------------------------------------------------------
// Entropy / Balloon
// ---------------------------------------------------------------------------

fn build_virtio_rng() -> crate::Result<Retained<VZEntropyDeviceConfiguration>> {
    unsafe {
        let config = VZVirtioEntropyDeviceConfiguration::new();
        Ok(Retained::into_super(config))
    }
}

fn build_virtio_balloon() -> crate::Result<Retained<VZMemoryBalloonDeviceConfiguration>> {
    unsafe {
        let config = VZVirtioTraditionalMemoryBalloonDeviceConfiguration::new();
        Ok(Retained::into_super(config))
    }
}
