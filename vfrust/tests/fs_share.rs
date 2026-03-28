mod common;

use std::time::Duration;
use vfrust::config::device::fs::VirtioFs;
use vfrust::config::device::Device;

#[tokio::test]
async fn test_virtio_fs_mount() {
    let disk = common::create_test_disk("fs-share");
    let iso = common::create_cloudinit_iso("fs-share");

    let share_dir = common::test_assets_dir().join("shared");
    let _ = std::fs::create_dir_all(&share_dir);
    std::fs::write(share_dir.join("hello.txt"), "from-host").expect("write test file");

    let config = common::efi_vm_config(
        &disk,
        Some(&iso),
        None,
        vec![Device::VirtioFs(VirtioFs {
            mount_tag: "hostshare".to_string(),
            shared_dir: Some(share_dir.clone()),
            directories: vec![],
        })],
    );
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let ip = common::find_vm_ip("fs-share", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");

    common::ssh_retry(
        &ip,
        "sudo mkdir -p /mnt/host && sudo mount -t virtiofs hostshare /mnt/host",
        Duration::from_secs(30),
    )
    .expect("mount virtiofs should succeed");

    let content = common::ssh_retry(&ip, "cat /mnt/host/hello.txt", Duration::from_secs(10))
        .expect("read shared file should succeed");
    assert_eq!(content, "from-host");

    common::stop_and_wait(&handle).await;
    let _ = std::fs::remove_dir_all(&share_dir);
}
