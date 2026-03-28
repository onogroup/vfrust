mod common;

use std::time::Duration;
use vfrust::VmState;

async fn wait_for_state(handle: &vfrust::VmHandle, target: VmState, timeout: Duration) {
    let mut rx = handle.state_stream();
    let result = tokio::time::timeout(timeout, async {
        loop {
            if *rx.borrow() == target {
                return;
            }
            rx.changed().await.ok();
        }
    })
    .await;
    assert!(result.is_ok(), "timed out waiting for {target:?}, got {:?}", handle.state());
}

#[tokio::test]
async fn test_efi_boot_and_start() {
    let disk = common::create_test_disk("efi-boot");
    let iso = common::create_cloudinit_iso("efi-boot");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);

    assert_eq!(handle.state(), VmState::Stopped);
    handle.start().await.expect("start VM");
    assert_eq!(handle.state(), VmState::Running);

    let ip = common::find_vm_ip("efi-boot", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");

    let output = common::ssh_retry(&ip, "echo hello", Duration::from_secs(30))
        .expect("ssh echo should succeed");
    assert_eq!(output, "hello");

    common::stop_and_wait(&handle).await;
    assert_eq!(handle.state(), VmState::Stopped);
}

#[tokio::test]
async fn test_efi_pause_resume() {
    let disk = common::create_test_disk("efi-pause");
    let iso = common::create_cloudinit_iso("efi-pause");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    let ip = common::find_vm_ip("efi-pause", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");

    handle.pause().await.expect("pause VM");
    assert_eq!(handle.state(), VmState::Paused);

    handle.resume().await.expect("resume VM");
    assert_eq!(handle.state(), VmState::Running);

    let output = common::ssh_retry(&ip, "echo resumed", Duration::from_secs(30))
        .expect("ssh after resume should succeed");
    assert_eq!(output, "resumed");

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_efi_force_stop() {
    let disk = common::create_test_disk("efi-fstop");
    let iso = common::create_cloudinit_iso("efi-fstop");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    assert_eq!(handle.state(), VmState::Running);

    common::find_vm_ip("efi-fstop", Duration::from_secs(180))
        .await
        .expect("VM should boot");

    handle.stop().await.expect("force stop");
    assert_eq!(handle.state(), VmState::Stopped);
}

#[tokio::test]
async fn test_efi_graceful_stop() {
    let disk = common::create_test_disk("efi-gstop");
    let iso = common::create_cloudinit_iso("efi-gstop");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    let ip = common::find_vm_ip("efi-gstop", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");

    handle.request_stop().await.expect("request stop");
    wait_for_state(&handle, VmState::Stopped, Duration::from_secs(30)).await;
}
