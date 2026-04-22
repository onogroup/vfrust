mod common;

use std::time::Duration;

use vfrust::config::device::network::MacAddress;
use vfrust::VmState;

/// Each test uses a distinct MAC so back-to-back runs do not alias in the
/// host's ARP/bridge cache — a stale entry from a previous test's shared
/// MAC would otherwise point `find_vm_ip` at a dead IP and spin until the
/// boot timeout.
const MAC_CPU: MacAddress = MacAddress([0x02, 0x00, 0xDE, 0xAD, 0xBE, 0xF1]);
const MAC_DELTA: MacAddress = MacAddress([0x02, 0x00, 0xDE, 0xAD, 0xBE, 0xF2]);
const MAC_STOPPED: MacAddress = MacAddress([0x02, 0x00, 0xDE, 0xAD, 0xBE, 0xF3]);
const MAC_PAUSE: MacAddress = MacAddress([0x02, 0x00, 0xDE, 0xAD, 0xBE, 0xF4]);
const MAC_CONCUR_A: MacAddress = MacAddress([0x02, 0x00, 0xAA, 0xAA, 0xAA, 0xAA]);
const MAC_CONCUR_B: MacAddress = MacAddress([0x02, 0x00, 0xBB, 0xBB, 0xBB, 0xBB]);

/// Boot the VM, give it a moment to schedule work, then assert that
/// `resource_usage()` reports non-zero CPU time for the VZ worker.
#[tokio::test]
async fn test_resource_usage_reports_cpu_after_boot() {
    let disk = common::create_test_disk("metrics-cpu");
    let iso = common::create_cloudinit_iso("metrics-cpu");

    let config = common::efi_vm_config_with_mac(&disk, Some(&iso), None, vec![], MAC_CPU);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    assert_eq!(handle.state(), VmState::Running);

    // Wait for boot so the hypervisor has actually done work.
    let ip = common::find_vm_ip_with_mac("metrics-cpu", MAC_CPU, Duration::from_secs(180))
        .await
        .expect("VM should boot");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");

    let usage = handle
        .resource_usage()
        .expect("resource_usage should be Some while running");
    assert!(
        usage.cpu_user_ns + usage.cpu_system_ns > 0,
        "expected non-zero cpu time, got {usage:?}"
    );
    assert!(
        usage.phys_footprint_bytes > 0,
        "expected non-zero phys footprint, got {usage:?}"
    );
    assert!(handle.worker_pid().is_some(), "worker_pid should be set");

    common::stop_and_wait(&handle).await;
}

/// `delta_since` between two samples taken under guest load should show
/// non-zero CPU time and disk write deltas.
#[tokio::test]
async fn test_delta_since_reports_cpu_and_disk_rate() {
    let disk = common::create_test_disk("metrics-delta");
    let iso = common::create_cloudinit_iso("metrics-delta");

    let config = common::efi_vm_config_with_mac(&disk, Some(&iso), None, vec![], MAC_DELTA);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    let ip = common::find_vm_ip_with_mac("metrics-delta", MAC_DELTA, Duration::from_secs(180))
        .await
        .expect("VM should boot");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");

    let first = handle.resource_usage().expect("first sample");

    // Produce measurable disk write + cpu work.
    common::ssh_retry(
        &ip,
        "dd if=/dev/urandom of=/tmp/x bs=1M count=64 conv=fsync 2>&1",
        Duration::from_secs(60),
    )
    .expect("dd ran");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let second = handle.resource_usage().expect("second sample");
    let delta = second.delta_since(&first).expect("delta");

    assert!(
        delta.cpu_user_ns + delta.cpu_system_ns > 0,
        "expected positive cpu delta, got {delta:?}"
    );
    assert!(
        delta.disk_write_bytes >= 16 * 1024 * 1024,
        "expected at least 16 MiB disk write delta, got {delta:?}"
    );
    assert!(delta.interval >= Duration::from_millis(500));

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_resource_usage_none_when_stopped() {
    let disk = common::create_test_disk("metrics-stopped");
    let iso = common::create_cloudinit_iso("metrics-stopped");

    let config = common::efi_vm_config_with_mac(&disk, Some(&iso), None, vec![], MAC_STOPPED);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);

    // Before start: no worker yet.
    assert!(
        handle.resource_usage().is_none(),
        "resource_usage should be None before start"
    );
    assert!(handle.worker_pid().is_none());

    handle.start().await.expect("start VM");
    common::find_vm_ip_with_mac("metrics-stopped", MAC_STOPPED, Duration::from_secs(180))
        .await
        .expect("VM should boot");

    assert!(
        handle.resource_usage().is_some(),
        "usage Some while running"
    );

    common::stop_and_wait(&handle).await;
    assert_eq!(handle.state(), VmState::Stopped);

    // Give the delegate event loop a moment to clear the worker slot.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
        handle.resource_usage().is_none(),
        "resource_usage should be None after stop"
    );
    assert!(
        handle.worker_pid().is_none(),
        "worker_pid should be None after stop"
    );
}

#[tokio::test]
async fn test_resource_usage_pid_stable_across_pause_resume() {
    let disk = common::create_test_disk("metrics-pause");
    let iso = common::create_cloudinit_iso("metrics-pause");

    let config = common::efi_vm_config_with_mac(&disk, Some(&iso), None, vec![], MAC_PAUSE);
    let vm = vfrust::VirtualMachine::new(config).expect("create VM");
    let handle = vm.handle();
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    let ip = common::find_vm_ip_with_mac("metrics-pause", MAC_PAUSE, Duration::from_secs(180))
        .await
        .expect("VM should boot");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");

    let pid_before = handle.worker_pid().expect("pid while running");

    handle.pause().await.expect("pause");
    assert_eq!(handle.state(), VmState::Paused);
    let pid_paused = handle.worker_pid().expect("pid while paused");
    assert_eq!(
        pid_before, pid_paused,
        "worker pid must be stable across pause"
    );

    handle.resume().await.expect("resume");
    assert_eq!(handle.state(), VmState::Running);
    let pid_after = handle.worker_pid().expect("pid after resume");
    assert_eq!(
        pid_before, pid_after,
        "worker pid must be stable across resume"
    );

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_resource_usage_distinct_pids_for_concurrent_vms() {
    let disk_a = common::create_test_disk("metrics-concur-a");
    let iso_a = common::create_cloudinit_iso("metrics-concur-a");
    let disk_b = common::create_test_disk("metrics-concur-b");
    let iso_b = common::create_cloudinit_iso("metrics-concur-b");

    let config_a =
        common::efi_vm_config_with_mac(&disk_a, Some(&iso_a), None, vec![], MAC_CONCUR_A);
    let config_b =
        common::efi_vm_config_with_mac(&disk_b, Some(&iso_b), None, vec![], MAC_CONCUR_B);

    let vm_a = vfrust::VirtualMachine::new(config_a).expect("create VM a");
    let vm_b = vfrust::VirtualMachine::new(config_b).expect("create VM b");
    let handle_a = vm_a.handle();
    let handle_b = vm_b.handle();
    let _guard_a = common::VmGuard::new(&handle_a);
    let _guard_b = common::VmGuard::new(&handle_b);

    // Issue the starts concurrently — this is the code path we care about.
    let (start_a, start_b) = tokio::join!(handle_a.start(), handle_b.start());
    start_a.expect("start VM a");
    start_b.expect("start VM b");

    assert_eq!(handle_a.state(), VmState::Running);
    assert_eq!(handle_b.state(), VmState::Running);

    let pid_a = handle_a.worker_pid().expect("pid a");
    let pid_b = handle_b.worker_pid().expect("pid b");
    assert_ne!(pid_a, pid_b, "concurrent VMs must get distinct worker pids");

    // Both should report usage independently.
    assert!(handle_a.resource_usage().is_some());
    assert!(handle_b.resource_usage().is_some());

    common::stop_and_wait(&handle_a).await;
    common::stop_and_wait(&handle_b).await;
}
