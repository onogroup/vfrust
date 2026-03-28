mod common;

use std::time::Duration;
use vfrust::VmState;

#[tokio::test]
async fn test_linux_direct_boot() {
    let kernel = common::ensure_raw_kernel();
    let initrd = common::ensure_alpine_initramfs();

    if !kernel.exists() {
        panic!("Raw ARM64 kernel Image not available. Install nix and run: nix build --system aarch64-linux nixpkgs#linuxPackages_latest.kernel");
    }

    let serial_log = common::test_assets_dir().join("linux-boot-serial.log");
    let _ = std::fs::remove_file(&serial_log);

    let config = common::linux_vm_config(&kernel, &initrd, &serial_log);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();

    assert_eq!(handle.state(), VmState::Stopped);
    handle.start().await.expect("start VM");
    assert_eq!(handle.state(), VmState::Running);

    let found =
        common::wait_for_file_content(&serial_log, "random: crng init done", Duration::from_secs(30))
            .await;
    assert!(found, "kernel boot messages not found in serial log");

    common::stop_and_wait(&handle).await;
    assert_eq!(handle.state(), VmState::Stopped);
}

#[tokio::test]
async fn test_linux_boot_serial_output() {
    let kernel = common::ensure_raw_kernel();
    let initrd = common::ensure_alpine_initramfs();

    if !kernel.exists() {
        panic!("Raw ARM64 kernel Image not available");
    }

    let serial_log = common::test_assets_dir().join("linux-serial-check.log");
    let _ = std::fs::remove_file(&serial_log);

    let config = common::linux_vm_config(&kernel, &initrd, &serial_log);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();

    handle.start().await.expect("start VM");

    let found =
        common::wait_for_file_content(&serial_log, "random: crng init done", Duration::from_secs(30))
            .await;
    assert!(found, "serial log should contain kernel boot messages");

    common::stop_and_wait(&handle).await;
}
