use std::collections::HashMap;
use std::path::PathBuf;

use vfrust::config::bootloader::{Bootloader, EfiBootloader, LinuxBootloader, MacOsBootloader};
use vfrust::config::device::fs::{Rosetta, SharedDir, VirtioFs};
use vfrust::config::device::audio::VirtioSound;
use vfrust::config::device::gpu::{MacGraphics, VirtioGpu};
use vfrust::config::device::input::VirtioInput;
use vfrust::config::device::network::{
    MacAddress, NetAttachment, VirtioNet, VmnetConfig, VmnetMode,
};
use vfrust::config::device::serial::{SerialAttachment, VirtioSerial};
use vfrust::config::device::storage::{
    DiskSyncMode, Nbd, Nvme, UsbMassStorage, VirtioBlk,
};
use vfrust::config::device::vsock::VirtioVsock;
use vfrust::config::device::Device;

fn parse_opts(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if s.is_empty() {
        return map;
    }
    for part in s.split(',') {
        if let Some((key, value)) = part.split_once('=') {
            map.insert(key.to_string(), value.to_string());
        } else {
            map.insert(part.to_string(), "true".to_string());
        }
    }
    map
}

pub fn parse_bootloader(spec: &str) -> Result<Bootloader, String> {
    let (kind, rest) = spec.split_once(',').unwrap_or((spec, ""));
    let opts = parse_opts(rest);

    match kind {
        "linux" => {
            let kernel = opts
                .get("kernel")
                .ok_or("linux bootloader requires 'kernel' option")?;
            Ok(Bootloader::Linux(LinuxBootloader {
                kernel_path: PathBuf::from(kernel),
                initrd_path: opts.get("initrd").map(PathBuf::from),
                command_line: opts.get("cmdline").cloned().unwrap_or_default(),
            }))
        }
        "efi" => {
            let store = opts
                .get("variable-store")
                .ok_or("efi bootloader requires 'variable-store' option")?;
            Ok(Bootloader::Efi(EfiBootloader {
                variable_store_path: PathBuf::from(store),
                create_variable_store: opts.contains_key("create"),
            }))
        }
        "macos" => {
            let machine_id = opts
                .get("machineIdentifierPath")
                .ok_or("macos bootloader requires 'machineIdentifierPath'")?;
            let hw_model = opts
                .get("hardwareModelPath")
                .ok_or("macos bootloader requires 'hardwareModelPath'")?;
            let aux = opts
                .get("auxImagePath")
                .ok_or("macos bootloader requires 'auxImagePath'")?;
            Ok(Bootloader::MacOs(MacOsBootloader {
                machine_identifier_path: PathBuf::from(machine_id),
                hardware_model_path: PathBuf::from(hw_model),
                aux_image_path: PathBuf::from(aux),
            }))
        }
        other => Err(format!("unknown bootloader type: {other}")),
    }
}

pub fn parse_device(spec: &str) -> Result<Device, String> {
    let (kind, rest) = spec.split_once(',').unwrap_or((spec, ""));
    let opts = parse_opts(rest);

    match kind {
        "virtio-blk" => {
            let path = opts
                .get("path")
                .ok_or("virtio-blk requires 'path' option")?;
            Ok(Device::VirtioBlk(VirtioBlk {
                path: PathBuf::from(path),
                read_only: opts.contains_key("readonly"),
                device_id: opts.get("deviceId").cloned(),
                ..Default::default()
            }))
        }
        "nvme" => {
            let path = opts.get("path").ok_or("nvme requires 'path' option")?;
            Ok(Device::Nvme(Nvme {
                path: PathBuf::from(path),
                read_only: opts.contains_key("readonly"),
            }))
        }
        "usb-mass-storage" => {
            let path = opts
                .get("path")
                .ok_or("usb-mass-storage requires 'path' option")?;
            Ok(Device::UsbMassStorage(UsbMassStorage {
                path: PathBuf::from(path),
                read_only: opts.contains_key("readonly"),
            }))
        }
        "nbd" => {
            let uri = opts.get("uri").ok_or("nbd requires 'uri' option")?;
            Ok(Device::Nbd(Nbd {
                uri: uri.clone(),
                device_id: opts.get("deviceId").cloned(),
                timeout: opts
                    .get("timeout")
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(std::time::Duration::from_millis),
                sync_mode: match opts.get("sync").map(|s| s.as_str()) {
                    Some("none") => DiskSyncMode::None,
                    Some("fsync") => DiskSyncMode::Fsync,
                    _ => DiskSyncMode::Full,
                },
                read_only: opts.contains_key("readonly"),
            }))
        }
        "virtio-net" => {
            let attachment = if opts.contains_key("nat") || opts.is_empty() {
                NetAttachment::Nat
            } else if let Some(path) = opts.get("unixSocketPath") {
                NetAttachment::UnixSocket {
                    path: PathBuf::from(path),
                }
            } else if let Some(fd) = opts.get("fd") {
                NetAttachment::FileDescriptor {
                    fd: fd.parse().map_err(|_| "invalid fd value")?,
                }
            } else if opts.contains_key("vmnet") {
                let mode = match opts.get("mode").map(|s| s.as_str()) {
                    Some("host") => VmnetMode::Host,
                    Some("bridged") => VmnetMode::Bridged,
                    _ => VmnetMode::Shared,
                };
                NetAttachment::Vmnet(VmnetConfig {
                    mode,
                    bridged_interface: opts.get("bridgedInterface").cloned(),
                    isolated: opts.contains_key("isolated"),
                    allocate_mac: opts.contains_key("allocateMac"),
                    ..VmnetConfig::default()
                })
            } else {
                NetAttachment::Nat
            };
            let mac_address = opts.get("mac").and_then(|s| MacAddress::parse(s));
            Ok(Device::VirtioNet(VirtioNet {
                attachment,
                mac_address,
            }))
        }
        "virtio-serial" => {
            let attachment = if opts.contains_key("stdio") || rest == "stdio" {
                SerialAttachment::Stdio
            } else if opts.contains_key("pty") || rest == "pty" {
                SerialAttachment::Pty
            } else if let Some(path) = opts.get("logFilePath") {
                SerialAttachment::File {
                    path: PathBuf::from(path),
                }
            } else {
                SerialAttachment::Stdio
            };
            Ok(Device::VirtioSerial(VirtioSerial { attachment }))
        }
        "virtio-vsock" => {
            let port = opts
                .get("port")
                .ok_or("virtio-vsock requires 'port' option")?
                .parse::<u32>()
                .map_err(|_| "invalid port value")?;
            Ok(Device::VirtioVsock(VirtioVsock {
                port,
                socket_url: opts.get("socketURL").cloned(),
                listen: opts.contains_key("listen"),
            }))
        }
        "virtio-gpu" => {
            let width = opts
                .get("width")
                .and_then(|s| s.parse().ok())
                .unwrap_or(800);
            let height = opts
                .get("height")
                .and_then(|s| s.parse().ok())
                .unwrap_or(600);
            Ok(Device::VirtioGpu(VirtioGpu { width, height }))
        }
        "virtio-input" => {
            if rest == "pointing" || opts.contains_key("pointing") {
                Ok(Device::VirtioInput(VirtioInput::Pointing))
            } else {
                Ok(Device::VirtioInput(VirtioInput::Keyboard))
            }
        }
        "virtio-fs" => {
            let mount_tag = opts
                .get("mountTag")
                .ok_or("virtio-fs requires 'mountTag' option")?
                .clone();

            // Collect directories: dir.<name>=<path> entries
            let mut directories: Vec<SharedDir> = Vec::new();
            for (key, value) in &opts {
                if let Some(name) = key.strip_prefix("dir.") {
                    directories.push(SharedDir {
                        name: name.to_string(),
                        path: PathBuf::from(value),
                        read_only: false,
                    });
                } else if let Some(name) = key.strip_prefix("rodir.") {
                    directories.push(SharedDir {
                        name: name.to_string(),
                        path: PathBuf::from(value),
                        read_only: true,
                    });
                }
            }

            let shared_dir = opts.get("sharedDir").map(|s| PathBuf::from(s));

            if shared_dir.is_none() && directories.is_empty() {
                return Err(
                    "virtio-fs requires 'sharedDir' or at least one 'dir.<name>=<path>'".into(),
                );
            }

            Ok(Device::VirtioFs(VirtioFs {
                shared_dir,
                mount_tag,
                directories,
            }))
        }
        "rosetta" => {
            let mount_tag = opts
                .get("mountTag")
                .cloned()
                .unwrap_or_else(|| "rosetta".to_string());
            Ok(Device::Rosetta(Rosetta {
                mount_tag,
                install: opts.contains_key("install"),
                ignore_if_missing: opts.contains_key("ignoreIfMissing"),
            }))
        }
        "virtio-sound" => {
            let input = opts.contains_key("input");
            let output = !opts.contains_key("no-output");
            Ok(Device::VirtioSound(VirtioSound { input, output }))
        }
        "mac-graphics" => {
            let width = opts
                .get("width")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1920);
            let height = opts
                .get("height")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1200);
            let pixels_per_inch = opts
                .get("pixelsPerInch")
                .and_then(|s| s.parse().ok())
                .unwrap_or(144);
            Ok(Device::MacGraphics(MacGraphics {
                width,
                height,
                pixels_per_inch,
            }))
        }
        "usb-controller" => Ok(Device::UsbController),
        "virtio-rng" => Ok(Device::VirtioRng),
        "virtio-balloon" => Ok(Device::VirtioBalloon),
        other => Err(format!("unknown device type: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_linux_bootloader() {
        let bl = parse_bootloader("linux,kernel=/tmp/vmlinuz,initrd=/tmp/initrd,cmdline=console=hvc0").unwrap();
        match bl {
            Bootloader::Linux(l) => {
                assert_eq!(l.kernel_path, PathBuf::from("/tmp/vmlinuz"));
                assert_eq!(l.initrd_path, Some(PathBuf::from("/tmp/initrd")));
                assert_eq!(l.command_line, "console=hvc0");
            }
            _ => panic!("expected Linux bootloader"),
        }
    }

    #[test]
    fn test_parse_efi_bootloader() {
        let bl = parse_bootloader("efi,variable-store=/tmp/efi.fd,create").unwrap();
        match bl {
            Bootloader::Efi(e) => {
                assert_eq!(e.variable_store_path, PathBuf::from("/tmp/efi.fd"));
                assert!(e.create_variable_store);
            }
            _ => panic!("expected EFI bootloader"),
        }
    }

    #[test]
    fn test_parse_virtio_blk() {
        let dev = parse_device("virtio-blk,path=/tmp/disk.img,readonly").unwrap();
        match dev {
            Device::VirtioBlk(blk) => {
                assert_eq!(blk.path, PathBuf::from("/tmp/disk.img"));
                assert!(blk.read_only);
            }
            _ => panic!("expected VirtioBlk"),
        }
    }

    #[test]
    fn test_parse_virtio_net_nat() {
        let dev = parse_device("virtio-net,nat").unwrap();
        match dev {
            Device::VirtioNet(net) => {
                assert!(matches!(net.attachment, NetAttachment::Nat));
            }
            _ => panic!("expected VirtioNet"),
        }
    }

    #[test]
    fn test_parse_virtio_serial_stdio() {
        let dev = parse_device("virtio-serial,stdio").unwrap();
        match dev {
            Device::VirtioSerial(s) => {
                assert!(matches!(s.attachment, SerialAttachment::Stdio));
            }
            _ => panic!("expected VirtioSerial"),
        }
    }

    #[test]
    fn test_parse_virtio_rng() {
        let dev = parse_device("virtio-rng").unwrap();
        assert!(matches!(dev, Device::VirtioRng));
    }

    #[test]
    fn test_parse_virtio_sound_defaults() {
        let dev = parse_device("virtio-sound").unwrap();
        match dev {
            Device::VirtioSound(s) => {
                assert!(!s.input);
                assert!(s.output);
            }
            _ => panic!("expected VirtioSound"),
        }
    }

    #[test]
    fn test_parse_virtio_sound_with_input() {
        let dev = parse_device("virtio-sound,input").unwrap();
        match dev {
            Device::VirtioSound(s) => {
                assert!(s.input);
                assert!(s.output);
            }
            _ => panic!("expected VirtioSound"),
        }
    }

    #[test]
    fn test_parse_virtio_sound_no_output() {
        let dev = parse_device("virtio-sound,input,no-output").unwrap();
        match dev {
            Device::VirtioSound(s) => {
                assert!(s.input);
                assert!(!s.output);
            }
            _ => panic!("expected VirtioSound"),
        }
    }

    #[test]
    fn test_parse_mac_graphics_defaults() {
        let dev = parse_device("mac-graphics").unwrap();
        match dev {
            Device::MacGraphics(g) => {
                assert_eq!(g.width, 1920);
                assert_eq!(g.height, 1200);
                assert_eq!(g.pixels_per_inch, 144);
            }
            _ => panic!("expected MacGraphics"),
        }
    }

    #[test]
    fn test_parse_mac_graphics_custom() {
        let dev =
            parse_device("mac-graphics,width=2560,height=1600,pixelsPerInch=220").unwrap();
        match dev {
            Device::MacGraphics(g) => {
                assert_eq!(g.width, 2560);
                assert_eq!(g.height, 1600);
                assert_eq!(g.pixels_per_inch, 220);
            }
            _ => panic!("expected MacGraphics"),
        }
    }

    #[test]
    fn test_parse_usb_controller() {
        let dev = parse_device("usb-controller").unwrap();
        assert!(matches!(dev, Device::UsbController));
    }

    #[test]
    fn test_parse_virtio_fs_single() {
        let dev = parse_device("virtio-fs,sharedDir=/tmp/share,mountTag=mytag").unwrap();
        match dev {
            Device::VirtioFs(fs) => {
                assert_eq!(fs.shared_dir, Some(PathBuf::from("/tmp/share")));
                assert_eq!(fs.mount_tag, "mytag");
                assert!(fs.directories.is_empty());
            }
            _ => panic!("expected VirtioFs"),
        }
    }

    #[test]
    fn test_parse_virtio_fs_multi_dirs() {
        let dev =
            parse_device("virtio-fs,mountTag=multi,dir.home=/home,dir.etc=/etc,rodir.logs=/var/log")
                .unwrap();
        match dev {
            Device::VirtioFs(fs) => {
                assert_eq!(fs.mount_tag, "multi");
                assert!(fs.shared_dir.is_none());
                assert_eq!(fs.directories.len(), 3);
                let home = fs.directories.iter().find(|d| d.name == "home").unwrap();
                assert_eq!(home.path, PathBuf::from("/home"));
                assert!(!home.read_only);
                let logs = fs.directories.iter().find(|d| d.name == "logs").unwrap();
                assert_eq!(logs.path, PathBuf::from("/var/log"));
                assert!(logs.read_only);
            }
            _ => panic!("expected VirtioFs"),
        }
    }

    #[test]
    fn test_parse_virtio_fs_requires_dir_or_shared() {
        let result = parse_device("virtio-fs,mountTag=empty");
        assert!(result.is_err());
    }
}
