mod common;

use std::time::Duration;
use vfrust::config::device::Device;

#[tokio::test]
async fn test_virtio_rng() {
    let disk = common::create_test_disk("dev-rng");
    let iso = common::create_cloudinit_iso("dev-rng");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let ip = common::find_vm_ip("dev-rng", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");

    let result = common::ssh_retry(
        &ip,
        "test -c /dev/hwrng && echo yes || echo no",
        Duration::from_secs(30),
    )
    .expect("ssh should succeed");
    assert_eq!(result, "yes", "/dev/hwrng should exist");

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_virtio_balloon() {
    let disk = common::create_test_disk("dev-bal");
    let iso = common::create_cloudinit_iso("dev-bal");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![Device::VirtioBalloon]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let ip = common::find_vm_ip("dev-bal", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");

    let result = common::ssh_retry(
        &ip,
        "test -d /sys/bus/virtio/drivers/virtio_balloon && echo yes || echo no",
        Duration::from_secs(30),
    )
    .expect("ssh should succeed");
    assert_eq!(result, "yes", "virtio-balloon driver should be present");

    common::stop_and_wait(&handle).await;
}
