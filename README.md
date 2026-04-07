# vfrust

A Rust library and CLI for managing macOS virtual machines using Apple's [Virtualization.framework](https://developer.apple.com/documentation/virtualization).

## Features

- **Linux, EFI, and macOS boot** — direct kernel boot, UEFI, or macOS bootloader
- **Virtio device stack** — block storage, NVMe, USB mass storage, NBD, networking (NAT / unix socket / fd passthrough), serial console, GPU, sound, filesystem sharing, vsock, RNG, balloon, input devices
- **Rosetta support** — run x86_64 Linux binaries in ARM VMs
- **VM lifecycle control** — start, pause, resume, stop, graceful ACPI shutdown, save/restore state
- **Thread-safe `VmHandle`** — control VMs from any thread via a dispatch-queue-backed handle
- **Vsock proxying** — bidirectional host↔guest communication over virtio-vsock, with unix socket bridging
- **Cloud-init** — automatic NoCloud ISO generation from user-data/meta-data files
- **Time synchronization** — optional host→guest time sync over vsock
- **Nested virtualization** — run VMs inside VMs on supported hardware
- **JSON config** — serialize/deserialize VM configurations

## Requirements

- macOS (Apple Silicon or Intel with Virtualization.framework support)
- Rust 1.70+
- Code signing with the `com.apple.security.virtualization` entitlement (handled by the Makefile)

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
| Virtio network | `virtio-net[,nat][,unixSocketPath=<path>][,fd=<n>][,mac=<addr>]` |
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

## Testing

```sh
make test-unit    # unit tests (no VM required)
make test-e2e     # end-to-end tests (creates real VMs, requires entitlements)
make test         # both
```

E2E tests are codesigned automatically and run single-threaded to avoid resource contention.

## Acknowledgments

This project was heavily inspired by [vfkit](https://github.com/crc-org/vfkit).
