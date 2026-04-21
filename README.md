# vfrust

A Rust library and CLI for managing macOS virtual machines using Apple's [Virtualization.framework](https://developer.apple.com/documentation/virtualization).

## Features

- **Linux, EFI, and macOS boot** — direct kernel boot, UEFI, or macOS bootloader
- **Virtio device stack** — block storage, NVMe, USB mass storage, NBD, networking (NAT / `vmnet.framework` / unix socket / fd passthrough), serial console, GPU, sound, filesystem sharing, vsock, RNG, balloon, input devices
- **Rosetta support** — run x86_64 Linux binaries in ARM VMs
- **VM lifecycle control** — start, pause, resume, stop, graceful ACPI shutdown, save/restore state
- **Thread-safe `VmHandle`** — control VMs from any thread via a dispatch-queue-backed handle
- **Vsock proxying** — bidirectional host↔guest communication over virtio-vsock, with unix socket bridging
- **Cloud-init** — automatic NoCloud ISO generation from user-data/meta-data files
- **Time synchronization** — optional host→guest time sync over vsock
- **Nested virtualization** — run VMs inside VMs on supported hardware
- **JSON config** — serialize/deserialize VM configurations
- **Metrics** — host-observed CPU / memory / disk / page-ins / energy per VM, sampled cheaply from the VZ worker subprocess; per-NIC byte / packet counters for `Vmnet` attachments

## Requirements

- macOS (Apple Silicon or Intel with Virtualization.framework support)
- Rust 1.70+
- Code signing with the `com.apple.security.virtualization` entitlement (handled by the Makefile)
- For `NetAttachment::Vmnet`: additionally the `com.apple.vm.networking` entitlement, **or** running as root (see [Entitlements](#entitlements))

## Workspace Structure

```
vfrust/          # Library crate — Virtualization.framework bindings and VM management API
vfrust-cli/      # CLI crate — command-line interface for creating and running VMs
```

## Quick Start

### Build and sign

```sh
make build        # debug build
make sign         # debug build + codesign with entitlements

make build-release
make sign-release # release build + codesign
```

The entitlement in `vfrust.entitlements` (`com.apple.security.virtualization`) is required for any binary that uses Virtualization.framework.

### Run a Linux VM

```sh
# Direct kernel boot with serial console
make run ARGS="--bootloader linux,kernel=/path/to/vmlinuz,initrd=/path/to/initrd.img,cmdline=console=hvc0 \
  --cpus 2 --memory 2048 \
  --device virtio-blk,path=/path/to/disk.img \
  --device virtio-net,nat \
  --device virtio-serial,stdio"
```

### Run an EFI VM

```sh
make run ARGS="--bootloader efi,variable-store=/path/to/efi-vars.fd,create \
  --cpus 4 --memory 4096 \
  --device nvme,path=/path/to/disk.img \
  --device virtio-net,nat \
  --gui"
```

### Cloud-init provisioning

```sh
make run ARGS="--bootloader linux,kernel=vmlinuz,initrd=initrd \
  --cloud-init /path/to/meta-data,/path/to/user-data \
  --device virtio-blk,path=disk.img \
  --device virtio-net,nat"
```

## CLI Reference

```
vfrust-cli [OPTIONS]
```

| Option | Description |
|---|---|
| `--cpus <N>` | Number of virtual CPUs (default: 1) |
| `--memory <MiB>` | Memory in MiB (default: 512, minimum: 128) |
| `--bootloader <SPEC>` | Bootloader specification (see below) |
| `--device <SPEC>` | Device specification (repeatable, see below) |
| `--gui` | Open a GUI window (auto-adds GPU + keyboard + pointing device) |
| `--cloud-init <FILES>` | Comma-separated cloud-init file paths (meta-data, user-data, network-config) |
| `--timesync <PORT>` | Enable host→guest time sync over vsock on the given port |
| `--metrics-interval <SECS>` | Print host-observed VM resource usage every N seconds |
| `--nested` | Enable nested virtualization |
| `--pidfile <PATH>` | Write PID to file (removed on exit) |
| `--log-level <LEVEL>` | Log level: debug, info, warn, error (default: info) |

### Bootloader specifications

```
linux,kernel=<path>[,initrd=<path>][,cmdline=<string>]
efi,variable-store=<path>[,create]
macos,machineIdentifierPath=<path>,hardwareModelPath=<path>,auxImagePath=<path>
```

### Device specifications

| Device | Syntax |
|---|---|
| Virtio block | `virtio-blk,path=<path>[,readonly][,deviceId=<id>]` |
| NVMe | `nvme,path=<path>[,readonly]` |
| USB mass storage | `usb-mass-storage,path=<path>[,readonly]` |
| NBD | `nbd,uri=<uri>[,deviceId=<id>][,timeout=<ms>][,sync=none\|fsync\|full][,readonly]` |
| Virtio network | `virtio-net[,nat][,vmnet[,mode=shared\|host\|bridged][,bridgedInterface=<iface>][,isolated][,allocateMac]][,unixSocketPath=<path>][,fd=<n>][,mac=<addr>]` |
| Virtio serial | `virtio-serial,stdio` / `virtio-serial,pty` / `virtio-serial,logFilePath=<path>` |
| Virtio vsock | `virtio-vsock,port=<n>[,socketURL=<path>][,listen]` |
| Virtio GPU | `virtio-gpu[,width=<n>][,height=<n>]` |
| Mac graphics | `mac-graphics[,width=<n>][,height=<n>][,pixelsPerInch=<n>]` |
| Virtio input | `virtio-input[,pointing]` (default: keyboard) |
| Virtio filesystem | `virtio-fs,mountTag=<tag>,sharedDir=<path>` or `virtio-fs,mountTag=<tag>,dir.<name>=<path>[,rodir.<name>=<path>]` |
| Rosetta | `rosetta[,mountTag=<tag>][,install][,ignoreIfMissing]` |
| Virtio sound | `virtio-sound[,input][,no-output]` |
| USB controller | `usb-controller` |
| Virtio RNG | `virtio-rng` |
| Virtio balloon | `virtio-balloon` |

## Library Usage

```rust
use vfrust::{VirtualMachine, VmConfig, Bootloader, LinuxBootloader, Device, VirtioBlk, VirtioNet, NetAttachment};

let config = VmConfig::builder()
    .cpus(2)
    .memory_mib(2048)
    .bootloader(Bootloader::Linux(LinuxBootloader {
        kernel_path: "/path/to/vmlinuz".into(),
        initrd_path: Some("/path/to/initrd".into()),
        command_line: "console=hvc0".into(),
    }))
    .device(Device::VirtioBlk(VirtioBlk {
        path: "/path/to/disk.img".into(),
        read_only: false,
        ..Default::default()
    }))
    .device(Device::VirtioNet(VirtioNet {
        attachment: NetAttachment::Nat,
        mac_address: None,
    }))
    .build()
    .unwrap();

let vm = VirtualMachine::new(config).unwrap();
let handle = vm.handle(); // Send + Sync, use from any thread

// handle.start().await
// handle.pause().await
// handle.resume().await
// handle.request_stop().await  — ACPI graceful shutdown
// handle.stop().await          — force stop
// handle.save_state(path).await
// handle.restore_state(path).await
```

## Networking

`NetAttachment` covers four modes:

| Variant | Description |
|---|---|
| `Nat` | Virtualization.framework's built-in NAT. Zero setup, no counters, no callbacks. |
| `Vmnet(VmnetConfig)` | Managed networking via Apple's `vmnet.framework`. vfrust owns the packet path, which enables per-NIC byte / packet counters (see [Metrics](#metrics)). Supports `Shared` (default, `192.168.64.0/24`), `Host` (`192.168.65.0/24`), and `Bridged` modes. Requires the `com.apple.vm.networking` entitlement or root. |
| `UnixSocket { path }` | Delegate the network path to an external unix socket. Caller owns the stack. |
| `FileDescriptor { fd }` | Delegate the network path to an existing fd. Caller owns the stack. |

```rust
use vfrust::{NetAttachment, VmnetConfig, VmnetMode, VirtioNet};

Device::VirtioNet(VirtioNet {
    attachment: NetAttachment::Vmnet(VmnetConfig {
        mode: VmnetMode::Shared,
        ..Default::default()
    }),
    mac_address: None, // let vfrust generate one, or set to pin
})
```

See the [`VmnetConfig`](vfrust/src/config/device/network.rs) docs for the
full set of knobs (DHCP range override, interface isolation, bridged-NIC
selection).

## Entitlements

Two entitlements are relevant:

| Entitlement | Required for |
|---|---|
| `com.apple.security.virtualization` | Every binary using Virtualization.framework — baseline. |
| `com.apple.vm.networking` | Any binary that uses `NetAttachment::Vmnet`. |

Both are present in `vfrust.entitlements` and applied by `make sign` /
`make test-e2e`. Downstream projects that embed vfrust must re-codesign
their own binaries with these entitlements, or run as root for ad-hoc
local development. Bridged-mode `Vmnet` in practice requires a
provisioning-profile-signed binary (Apple Developer program); Shared and
Host modes work with the ad-hoc `com.apple.vm.networking` entitlement or
with root.

## Metrics

Sample host-observed VM resource usage from a running VM:

```rust
use vfrust::{VirtualMachine, VmConfig};

let vm = VirtualMachine::new(config)?;
let handle = vm.handle();
handle.start().await?;

if let Some(usage) = handle.resource_usage() {
    println!("{usage}");
    // cpu=1.23s mem=512MiB disk=r:12MiB/w:4MiB
}
```

`ResourceUsage` reports CPU time, memory footprint, disk I/O, page-ins, and
(on Apple Silicon) billed energy and CPU perf counters for the
`com.apple.Virtualization.VirtualMachine` worker subprocess that backs each
VM — the same source Activity Monitor reads.

The values are **host-observed**, not guest-internal: CPU time is real host
time the hypervisor consumed, memory is host backing footprint (≈ allocated
physical minus ballooned pages, not guest-free memory), and disk I/O is the
aggregate across all image attachments. On Intel Macs the `energy_nj`,
`instructions`, and `cycles` fields are always `0`.

Use `ResourceUsage::delta_since` to compute per-interval rates from two
samples cheaply; see the [`ResourceUsage`](vfrust/src/vm/metrics.rs) doc
comment for the full caveat list.

### Per-NIC network counters (`Vmnet` only)

Unlike CPU/memory/disk, network counters are **not** exposed by
Virtualization.framework. vfrust gets them by owning the packet path for
`NetAttachment::Vmnet` attachments — every packet traverses a userspace
`vmnet` ↔ `socketpair` proxy, and byte / packet counts are incremented
there.

```rust
let usage = handle.network_usage();   // Vec<NetworkUsage>, one per Vmnet NIC
for (i, nic) in usage.iter().enumerate() {
    println!("nic{i}: {nic}"); // rx=12MiB/458pkt tx=3MiB/127pkt
}
```

`Nat`, `UnixSocket`, and `FileDescriptor` attachments do not go through
the proxy and produce no entries in `network_usage()`. Callers of those
variants can count bytes at their own layer. See
[`NetworkUsage`](vfrust/src/vm/network_metrics.rs) for the full shape
and the `delta_since` helper.

## Testing

```sh
make test-unit    # unit tests (no VM required)
make test-e2e     # end-to-end tests (creates real VMs, requires entitlements)
make test         # both
```

E2E tests are codesigned automatically and run single-threaded to avoid resource contention.

## Acknowledgments

This project was heavily inspired by [vfkit](https://github.com/crc-org/vfkit).
