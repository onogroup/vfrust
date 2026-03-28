mod common;

use std::time::Duration;
use vfrust::config::device::storage::VirtioBlk;
use vfrust::config::device::Device;
use vfrust::VmState;

#[tokio::test]
async fn test_virtio_blk_readwrite() {
    let disk = common::create_test_disk("stor-rw");
    let iso = common::create_cloudinit_iso("stor-rw");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let ip = common::find_vm_ip("stor-rw", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");

    common::ssh_retry(
        &ip,
        "echo vfrust-persistence-test | sudo tee /var/marker.txt && sync",
        Duration::from_secs(30),
    )
    .expect("write marker should succeed");

    // Graceful shutdown to flush disk
    handle.request_stop().await.expect("request stop");
    let mut rx = handle.state_stream();
    let _ = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if *rx.borrow() == VmState::Stopped {
                break;
            }
            rx.changed().await.ok();
        }
    })
    .await;
    drop(vm);

    // Second boot from same disk
    let config2 = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm2 = vfrust::VirtualMachine::new(config2).expect("create VM (2nd boot)");
    let handle2 = vm2.handle();
    let _guard2 = common::VmGuard::new(&handle2);
    handle2.start().await.expect("start VM (2nd boot)");

    let ip2 = common::find_vm_ip("stor-rw", Duration::from_secs(180))
        .await
        .expect("VM should get an IP (2nd boot)");

    let content = common::ssh_retry(
        &ip2,
        "cat /var/marker.txt 2>/dev/null || echo MISSING",
        Duration::from_secs(30),
    )
    .expect("read marker should succeed");
    assert_eq!(content, "vfrust-persistence-test");

    common::stop_and_wait(&handle2).await;
}

#[tokio::test]
async fn test_additional_disk() {
    let disk = common::create_test_disk("stor-ext");
    let iso = common::create_cloudinit_iso("stor-ext");

    // Create a 100MB extra disk
    let extra_disk_path = common::test_assets_dir().join("extra-disk.raw");
    let _ = std::fs::remove_file(&extra_disk_path);
    let f = std::fs::File::create(&extra_disk_path).expect("create extra disk");
    f.set_len(100 * 1024 * 1024).expect("set disk size");
    drop(f);
    let _extra_guard = common::TestFile(extra_disk_path.clone());

    let config = common::efi_vm_config(
        &disk,
        Some(&iso),
        None,
        vec![Device::VirtioBlk(VirtioBlk {
            path: extra_disk_path,
            ..Default::default()
        })],
    );
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let ip = common::find_vm_ip("stor-ext", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");

    let result = common::ssh_retry(
        &ip,
        "lsblk -d -n -o NAME | sort | tail -1",
        Duration::from_secs(30),
    )
    .expect("ssh lsblk should succeed");
    assert!(
        result.contains("vdb") || result.contains("sdb"),
        "second disk should be visible, got: {result}"
    );

    common::stop_and_wait(&handle).await;
}

