mod common;

use std::time::Duration;

#[tokio::test]
async fn test_cloudinit_hostname() {
    let disk = common::create_test_disk("ci-host");
    let iso = common::create_cloudinit_iso("ci-host");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    // find_vm_ip already verifies hostname matches "ci-host"
    let ip = common::find_vm_ip("ci-host", Duration::from_secs(180))
        .await
        .expect("VM should get an IP with correct hostname");

    let hostname = common::ssh_command(&ip, "hostname")
        .expect("ssh hostname should succeed");
    assert_eq!(hostname, "ci-host");

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_cloudinit_user_creation() {
    let disk = common::create_test_disk("ci-user");
    let iso = common::create_cloudinit_iso("ci-user");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let ip = common::find_vm_ip("ci-user", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");

    let shell = common::ssh_retry(
        &ip,
        "getent passwd ubuntu | cut -d: -f7",
        Duration::from_secs(30),
    )
    .expect("ssh getent should succeed");
    assert_eq!(shell, "/bin/bash");

    common::stop_and_wait(&handle).await;
}
