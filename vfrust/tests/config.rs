use vfrust::config::bootloader::{Bootloader, EfiBootloader, LinuxBootloader};
use vfrust::config::device::network::{MacAddress, NetAttachment, VirtioNet};
use vfrust::config::device::serial::{SerialAttachment, VirtioSerial};
use vfrust::config::device::storage::VirtioBlk;
use vfrust::config::device::Device;
use vfrust::{VmConfig, VmState};

fn sample_config() -> VmConfig {
    VmConfig::builder()
        .cpus(4)
        .memory_mib(2048)
        .bootloader(Bootloader::Linux(LinuxBootloader {
            kernel_path: "/tmp/vmlinuz".into(),
            initrd_path: Some("/tmp/initrd".into()),
            command_line: "console=hvc0 root=/dev/vda1".into(),
        }))
        .device(Device::VirtioBlk(VirtioBlk {
            path: "/tmp/disk.img".into(),
            ..Default::default()
        }))
        .device(Device::VirtioNet(VirtioNet {
            attachment: NetAttachment::Nat,
            mac_address: Some(MacAddress([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])),
        }))
        .device(Device::VirtioSerial(VirtioSerial {
            attachment: SerialAttachment::Stdio,
        }))
        .device(Device::VirtioRng)
        .device(Device::VirtioBalloon)
        .nested(true)
        .build()
        .unwrap()
}

#[test]
fn test_json_roundtrip() {
    let config = sample_config();
    let json = config.to_json().expect("serialize");
    assert!(json.contains("\"cpus\": 4"));
    assert!(json.contains("\"memory_mib\": 2048"));
    assert!(json.contains("console=hvc0"));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, &json).unwrap();
    let loaded = VmConfig::from_json(&path).expect("deserialize");

    assert_eq!(loaded.cpus(), config.cpus());
    assert_eq!(loaded.memory_mib(), config.memory_mib());
    assert_eq!(loaded.nested(), config.nested());
    assert_eq!(loaded.devices().len(), config.devices().len());
}

#[test]
fn test_config_validation_rejects_no_bootloader() {
    let result = VmConfig::builder().build();
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("bootloader is required"));
}

#[test]
fn test_config_empty_devices_valid() {
    let config = VmConfig::builder()
        .bootloader(Bootloader::Linux(LinuxBootloader {
            kernel_path: "/tmp/vmlinuz".into(),
            initrd_path: None,
            command_line: String::new(),
        }))
        .build()
        .unwrap();
    assert!(config.devices().is_empty());
}

#[test]
fn test_config_multiple_same_device_type() {
    let config = VmConfig::builder()
        .bootloader(Bootloader::Linux(LinuxBootloader {
            kernel_path: "/tmp/vmlinuz".into(),
            initrd_path: None,
            command_line: String::new(),
        }))
        .device(Device::VirtioBlk(VirtioBlk {
            path: "/tmp/disk1.img".into(),
            ..Default::default()
        }))
        .device(Device::VirtioBlk(VirtioBlk {
            path: "/tmp/disk2.img".into(),
            ..Default::default()
        }))
        .build()
        .unwrap();
    assert_eq!(config.devices().len(), 2);
}

#[test]
fn test_vm_state_predicates() {
    assert!(VmState::Stopped.can_start());
    assert!(!VmState::Stopped.can_pause());
    assert!(!VmState::Stopped.can_resume());
    assert!(!VmState::Stopped.can_stop());

    assert!(!VmState::Running.can_start());
    assert!(VmState::Running.can_pause());
    assert!(VmState::Running.can_stop());
    assert!(VmState::Running.can_request_stop());

    assert!(!VmState::Paused.can_start());
    assert!(VmState::Paused.can_resume());
    assert!(VmState::Paused.can_stop());
    assert!(!VmState::Paused.can_request_stop());

    for state in [
        VmState::Starting,
        VmState::Pausing,
        VmState::Resuming,
        VmState::Stopping,
        VmState::Error,
    ] {
        assert!(!state.can_start(), "{state:?}");
        assert!(!state.can_pause(), "{state:?}");
        assert!(!state.can_resume(), "{state:?}");
    }
}

#[test]
fn test_mac_address_parse_valid() {
    let mac = MacAddress::parse("aa:bb:cc:dd:ee:ff").unwrap();
    assert_eq!(mac.0, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
    assert_eq!(mac.to_string(), "aa:bb:cc:dd:ee:ff");
}

#[test]
fn test_mac_address_parse_invalid() {
    assert!(MacAddress::parse("").is_none());
    assert!(MacAddress::parse("aa:bb:cc").is_none());
    assert!(MacAddress::parse("gg:hh:ii:jj:kk:ll").is_none());
}

#[test]
fn test_mac_address_display_roundtrip() {
    let original = "01:23:45:ab:cd:ef";
    let mac = MacAddress::parse(original).unwrap();
    assert_eq!(mac.to_string(), original);
}

#[test]
fn test_efi_bootloader_config() {
    let config = VmConfig::builder()
        .bootloader(Bootloader::Efi(EfiBootloader {
            variable_store_path: "/tmp/efi.fd".into(),
            create_variable_store: true,
        }))
        .build()
        .unwrap();
    match config.bootloader() {
        Bootloader::Efi(efi) => assert!(efi.create_variable_store),
        _ => panic!("expected EFI bootloader"),
    }
}
