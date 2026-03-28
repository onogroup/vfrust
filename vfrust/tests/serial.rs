mod common;

use std::time::Duration;

#[tokio::test]
async fn test_serial_file_output() {
    let disk = common::create_test_disk("serial-f");
    let iso = common::create_cloudinit_iso("serial-f");
    let serial_log = common::test_assets_dir().join("serial-file-test.log");
    let _ = std::fs::remove_file(&serial_log);

    let config = common::efi_vm_config(&disk, Some(&iso), Some(&serial_log), vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let found =
        common::wait_for_file_content(&serial_log, "login:", Duration::from_secs(180)).await;

    let content = std::fs::read_to_string(&serial_log).unwrap_or_default();
    assert!(
        found,
        "serial log should contain login prompt, got {} bytes: {:?}",
        content.len(),
        &content[..content.len().min(200)]
    );

    common::stop_and_wait(&handle).await;
}
