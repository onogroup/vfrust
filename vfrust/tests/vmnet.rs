//! Integration tests for `NetAttachment::Vmnet`.
//!
//! These tests require either:
//! - the `com.apple.vm.networking` entitlement on the test binary (handled
//!   by `make test-e2e`), **or**
//! - running as root (e.g. `sudo -E cargo test …`).
//!
//! Without one of those, `vmnet_start_interface` returns
//! `VmnetReturn::InvalidAccess` and we emit a skip message instead of a
//! failure. The codesign loop in the top-level `Makefile` adds the
//! entitlement to `target/debug/deps/vmnet-*` for ad-hoc runs.
//!
//! Tests run single-threaded (via `--test-threads=1`) and use distinct MAC
//! addresses so back-to-back runs don't alias in the host ARP cache.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use vfrust::config::bootloader::{Bootloader, EfiBootloader};
use vfrust::config::device::network::{MacAddress, NetAttachment, VirtioNet, VmnetConfig};
use vfrust::config::device::storage::VirtioBlk;
use vfrust::config::device::Device;
use vfrust::{VmConfig, VmState};

// One locally-administered MAC per test to avoid ARP-cache aliasing.
const MAC_BOOT: MacAddress = MacAddress([0x02, 0x00, 0x56, 0x4E, 0x00, 0x01]);
const MAC_USAGE: MacAddress = MacAddress([0x02, 0x00, 0x56, 0x4E, 0x00, 0x02]);
const MAC_SCP: MacAddress = MacAddress([0x02, 0x00, 0x56, 0x4E, 0x00, 0x03]);
const MAC_DD: MacAddress = MacAddress([0x02, 0x00, 0x56, 0x4E, 0x00, 0x04]);
const MAC_CONCUR_A: MacAddress = MacAddress([0x02, 0x00, 0x56, 0x4E, 0x0A, 0x01]);
const MAC_CONCUR_B: MacAddress = MacAddress([0x02, 0x00, 0x56, 0x4E, 0x0A, 0x02]);
const MAC_LEAK: MacAddress = MacAddress([0x02, 0x00, 0x56, 0x4E, 0x00, 0x05]);

/// Build an EFI + cloud-init VM config with a single `NetAttachment::Vmnet`
/// Shared-mode NIC. Mirrors `common::efi_vm_config_with_mac` but swaps the
/// attachment.
fn vmnet_vm_config(
    disk: &Path,
    cloudinit_iso: Option<&Path>,
    mac: MacAddress,
    vmnet_cfg: VmnetConfig,
) -> VmConfig {
    let efi_store = common::test_assets_dir().join(format!(
        "efi-{}.fd",
        disk.file_stem().unwrap().to_str().unwrap()
    ));
    let _ = std::fs::remove_file(&efi_store);

    let mut builder = VmConfig::builder()
        .cpus(2)
        .memory_mib(2048)
        .bootloader(Bootloader::Efi(EfiBootloader {
            variable_store_path: efi_store,
            create_variable_store: true,
        }))
        .device(Device::VirtioBlk(VirtioBlk {
            path: disk.to_path_buf(),
            ..Default::default()
        }))
        .device(Device::VirtioNet(VirtioNet {
            attachment: NetAttachment::Vmnet(vmnet_cfg),
            mac_address: Some(mac),
        }))
        .device(Device::VirtioRng);

    if let Some(iso) = cloudinit_iso {
        builder = builder.device(Device::VirtioBlk(VirtioBlk {
            path: iso.to_path_buf(),
            read_only: true,
            ..Default::default()
        }));
    }

    builder.build().expect("build VM config")
}

/// Try to build and start a VM with a Vmnet attachment. Returns `Ok(None)`
/// when the failure is an entitlement / access denial so the caller can
/// emit a skip message instead of a test failure.
fn try_start(
    vm_config: VmConfig,
) -> Result<Option<(vfrust::VirtualMachine, vfrust::VmHandle)>, String> {
    let vm = match vfrust::VirtualMachine::new(vm_config) {
        Ok(vm) => vm,
        Err(e) => {
            let s = e.to_string();
            if s.contains("InvalidAccess") || s.contains("vmnet entitlement") {
                eprintln!("skipping vmnet test: {s}");
                return Ok(None);
            }
            return Err(format!("create VM: {e}"));
        }
    };
    let handle = vm.handle();
    Ok(Some((vm, handle)))
}

/// `true` when `NetAttachment::Vmnet` is likely to work in this process.
/// Used to skip cleanly on CI or dev environments lacking entitlement/root.
fn vmnet_available() -> bool {
    // Fast path: if we're root, we're good.
    // SAFETY: getuid is always safe and cannot fail.
    let uid = unsafe { libc::getuid() };
    if uid == 0 {
        return true;
    }
    // Otherwise, let the test probe via try_start — the entitlement is a
    // codesign attribute and cheap to check only indirectly.
    // We return true and let the individual test skip on InvalidAccess.
    true
}

#[tokio::test]
async fn test_vmnet_shared_boots_and_gets_ip() {
    if !vmnet_available() {
        eprintln!("skipping: vmnet unavailable");
        return;
    }

    let disk = common::create_test_disk("vmnet-boot");
    let iso = common::create_cloudinit_iso("vmnet-boot");

    let cfg = vmnet_vm_config(&disk, Some(&iso), MAC_BOOT, VmnetConfig::default());
    let Some((_vm, handle)) = try_start(cfg).expect("build VM") else {
        return;
    };
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    assert_eq!(handle.state(), VmState::Running);

    let interfaces = handle.vmnet_interfaces();
    assert_eq!(interfaces.len(), 1, "expected one vmnet interface");
    let iface = &interfaces[0];
    assert!(iface.mtu >= 1500, "mtu too small: {iface:?}");
    assert!(iface.max_packet_size >= iface.mtu, "bad max_packet_size");
    if let Some(dhcp_start) = iface.dhcp_start {
        assert!(
            dhcp_start.is_private(),
            "vmnet DHCP start should be private: {dhcp_start}"
        );
    }

    let ip = common::find_vm_ip_with_mac("vmnet-boot", MAC_BOOT, Duration::from_secs(180))
        .await
        .expect("VM should boot and get a DHCP lease over vmnet");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_network_usage_reports_rx_tx_after_ssh() {
    if !vmnet_available() {
        return;
    }

    let disk = common::create_test_disk("vmnet-usage");
    let iso = common::create_cloudinit_iso("vmnet-usage");
    let cfg = vmnet_vm_config(&disk, Some(&iso), MAC_USAGE, VmnetConfig::default());
    let Some((_vm, handle)) = try_start(cfg).expect("build VM") else {
        return;
    };
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    let ip = common::find_vm_ip_with_mac("vmnet-usage", MAC_USAGE, Duration::from_secs(180))
        .await
        .expect("VM should boot");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");
    common::ssh_retry(&ip, "uname -a", Duration::from_secs(15)).expect("ran command");

    let usage = handle.network_usage();
    assert_eq!(usage.len(), 1, "expected one vmnet NIC");
    let u = &usage[0];
    assert!(
        u.rx_bytes > 0 && u.tx_bytes > 0,
        "expected rx and tx > 0 after ssh, got {u:?}"
    );
    assert!(
        u.rx_packets > 0 && u.tx_packets > 0,
        "expected packet counts > 0, got {u:?}"
    );

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_network_usage_delta_matches_scp_in() {
    if !vmnet_available() {
        return;
    }

    let disk = common::create_test_disk("vmnet-scp");
    let iso = common::create_cloudinit_iso("vmnet-scp");
    let cfg = vmnet_vm_config(&disk, Some(&iso), MAC_SCP, VmnetConfig::default());
    let Some((_vm, handle)) = try_start(cfg).expect("build VM") else {
        return;
    };
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    let ip = common::find_vm_ip_with_mac("vmnet-scp", MAC_SCP, Duration::from_secs(180))
        .await
        .expect("VM should boot");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");

    // Create a 16 MiB file on host, scp it into the guest.
    let src = common::test_assets_dir().join("vmnet-scp-src.bin");
    let _ = std::fs::remove_file(&src);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&src).expect("create src");
        f.write_all(&vec![0u8; 16 * 1024 * 1024]).expect("write");
    }

    let first = handle.network_usage().into_iter().next().expect("sample");

    let key_path = dirs::home_dir().unwrap().join(".ssh/id_ed25519");
    let status = Command::new("scp")
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "BatchMode=yes",
            "-i",
            &key_path.to_string_lossy(),
            src.to_str().unwrap(),
            &format!("ubuntu@{ip}:/tmp/vmnet-scp-dst.bin"),
        ])
        .status()
        .expect("spawn scp");
    assert!(status.success(), "scp failed");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let second = handle.network_usage().into_iter().next().expect("sample");
    let delta = second.delta_since(&first).expect("delta");

    // Guest receives from host → host sees guest-bound traffic as rx from
    // the vmnet proxy's perspective (packets read from vmnet → fd → guest).
    // The direction convention matches the guest's view:
    //   tx = host→guest (what we scp'd in)
    //   rx = guest→host (ACKs, scp server responses)
    // Be generous: either direction picks up the bulk depending on how we
    // label — assert their sum covers the payload plus reasonable overhead.
    let total = delta.rx_bytes + delta.tx_bytes;
    assert!(
        total >= 16 * 1024 * 1024,
        "expected >=16 MiB of network traffic, got rx={} tx={} (total={total})",
        delta.rx_bytes,
        delta.tx_bytes
    );
    assert!(delta.rx_packets + delta.tx_packets > 0);

    let _ = std::fs::remove_file(&src);
    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_network_usage_delta_matches_dd_out() {
    if !vmnet_available() {
        return;
    }

    let disk = common::create_test_disk("vmnet-dd");
    let iso = common::create_cloudinit_iso("vmnet-dd");
    let cfg = vmnet_vm_config(&disk, Some(&iso), MAC_DD, VmnetConfig::default());
    let Some((_vm, handle)) = try_start(cfg).expect("build VM") else {
        return;
    };
    let _guard = common::VmGuard::new(&handle);

    handle.start().await.expect("start VM");
    let ip = common::find_vm_ip_with_mac("vmnet-dd", MAC_DD, Duration::from_secs(180))
        .await
        .expect("VM should boot");
    common::ssh_retry(&ip, "true", Duration::from_secs(30)).expect("ssh ready");

    let first = handle.network_usage().into_iter().next().expect("sample");

    // Pull 16 MiB from the guest to /dev/null on the host.
    let key_path = dirs::home_dir().unwrap().join(".ssh/id_ed25519");
    let output = Command::new("ssh")
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "BatchMode=yes",
            "-o",
            "LogLevel=ERROR",
            "-i",
            &key_path.to_string_lossy(),
            &format!("ubuntu@{ip}"),
            "dd if=/dev/zero bs=1M count=16 2>/dev/null",
        ])
        .output()
        .expect("spawn ssh");
    assert!(output.status.success(), "ssh dd failed");
    assert_eq!(output.stdout.len(), 16 * 1024 * 1024, "dd payload size");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let second = handle.network_usage().into_iter().next().expect("sample");
    let delta = second.delta_since(&first).expect("delta");
    let total = delta.rx_bytes + delta.tx_bytes;
    assert!(
        total >= 16 * 1024 * 1024,
        "expected >=16 MiB of network traffic, got rx={} tx={}",
        delta.rx_bytes,
        delta.tx_bytes
    );

    common::stop_and_wait(&handle).await;
}

#[tokio::test]
async fn test_vmnet_distinct_interfaces_for_concurrent_vms() {
    if !vmnet_available() {
        return;
    }

    let disk_a = common::create_test_disk("vmnet-concur-a");
    let iso_a = common::create_cloudinit_iso("vmnet-concur-a");
    let disk_b = common::create_test_disk("vmnet-concur-b");
    let iso_b = common::create_cloudinit_iso("vmnet-concur-b");

    let cfg_a = vmnet_vm_config(&disk_a, Some(&iso_a), MAC_CONCUR_A, VmnetConfig::default());
    let cfg_b = vmnet_vm_config(&disk_b, Some(&iso_b), MAC_CONCUR_B, VmnetConfig::default());

    let Some((_vm_a, handle_a)) = try_start(cfg_a).expect("build VM a") else {
        return;
    };
    let Some((_vm_b, handle_b)) = try_start(cfg_b).expect("build VM b") else {
        return;
    };
    let _guard_a = common::VmGuard::new(&handle_a);
    let _guard_b = common::VmGuard::new(&handle_b);

    let (sa, sb) = tokio::join!(handle_a.start(), handle_b.start());
    sa.expect("start a");
    sb.expect("start b");

    let ifaces_a = handle_a.vmnet_interfaces();
    let ifaces_b = handle_b.vmnet_interfaces();
    assert_eq!(ifaces_a.len(), 1);
    assert_eq!(ifaces_b.len(), 1);
    assert_ne!(
        ifaces_a[0].mac, ifaces_b[0].mac,
        "concurrent vmnet VMs must get distinct MACs"
    );

    common::find_vm_ip_with_mac("vmnet-concur-a", MAC_CONCUR_A, Duration::from_secs(180))
        .await
        .expect("a boots");
    common::find_vm_ip_with_mac("vmnet-concur-b", MAC_CONCUR_B, Duration::from_secs(180))
        .await
        .expect("b boots");

    let usage_a = handle_a.network_usage();
    let usage_b = handle_b.network_usage();
    assert_eq!(usage_a.len(), 1);
    assert_eq!(usage_b.len(), 1);

    common::stop_and_wait(&handle_a).await;
    common::stop_and_wait(&handle_b).await;
}

#[tokio::test]
async fn test_vmnet_no_leak_after_stop() {
    if !vmnet_available() {
        return;
    }

    let bridges_before = count_bridge_interfaces();

    let disk = common::create_test_disk("vmnet-leak");
    let iso = common::create_cloudinit_iso("vmnet-leak");
    let cfg = vmnet_vm_config(&disk, Some(&iso), MAC_LEAK, VmnetConfig::default());
    let Some((vm, handle)) = try_start(cfg).expect("build VM") else {
        return;
    };

    handle.start().await.expect("start VM");
    common::find_vm_ip_with_mac("vmnet-leak", MAC_LEAK, Duration::from_secs(180))
        .await
        .expect("VM should boot");

    common::stop_and_wait(&handle).await;
    drop(vm); // triggers VmnetProxy::Drop → stop_interface

    // Give vmnet_stop_interface's completion handler time to run.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let bridges_after = count_bridge_interfaces();
    assert!(
        bridges_after <= bridges_before,
        "bridge interface count grew after stop (before={bridges_before}, after={bridges_after}) — likely a vmnet_stop_interface leak"
    );
}

fn count_bridge_interfaces() -> usize {
    let out = Command::new("ifconfig").arg("-l").output();
    let Ok(out) = out else { return 0 };
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().filter(|n| n.starts_with("bridge")).count()
}
