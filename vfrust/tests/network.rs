mod common;

use std::time::Duration;

#[tokio::test]
async fn test_nat_networking() {
    let disk = common::create_test_disk("net-nat");
    let iso = common::create_cloudinit_iso("net-nat");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let ip = common::find_vm_ip("net-nat", Duration::from_secs(180))
        .await
        .expect("VM should get a NAT IP");

    assert!(
        ip.starts_with("192.168.64."),
        "VM IP {ip} should be in 192.168.64.0/24"
    );

    let output = common::ssh_retry(&ip, "echo ok", Duration::from_secs(30))
        .expect("ssh should succeed");
    assert_eq!(output, "ok");

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_ssh_via_cloudinit() {
    let disk = common::create_test_disk("ssh-test");
    let iso = common::create_cloudinit_iso("ssh-test");

    let config = common::efi_vm_config(&disk, Some(&iso), None, vec![]);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);
    handle.start().await.expect("start VM");

    let ip = common::find_vm_ip("ssh-test", Duration::from_secs(180))
        .await
        .expect("VM should get an IP");

    let whoami = common::ssh_retry(&ip, "whoami", Duration::from_secs(30))
        .expect("ssh whoami should succeed");
    assert_eq!(whoami, "ubuntu");

    let sudo_test = common::ssh_retry(&ip, "sudo whoami", Duration::from_secs(30))
        .expect("ssh sudo should succeed");
    assert_eq!(sudo_test, "root");

    common::stop_and_wait(&handle).await;
}
