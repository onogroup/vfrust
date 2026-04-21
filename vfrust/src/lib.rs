pub mod config;
pub mod error;
pub mod vm;
pub mod vsock;
mod sys;

pub use config::{
    bootloader::{Bootloader, EfiBootloader, LinuxBootloader, MacOsBootloader},
    device::{
        audio::VirtioSound,
        fs::{Rosetta, SharedDir, VirtioFs},
        gpu::{MacGraphics, VirtioGpu},
        input::VirtioInput,
        network::{MacAddress, NetAttachment, VirtioNet, VmnetConfig, VmnetMode},
        serial::{SerialAttachment, VirtioSerial},
        storage::{DiskBackend, DiskCachingMode, DiskSyncMode, Nbd, Nvme, UsbMassStorage, VirtioBlk},
        vsock::VirtioVsock,
        Device,
    },
    platform::{MachineIdentifier, MacOsPlatform, Platform},
    vm::{VmBuilder, VmConfig},
};
pub use error::{Error, Result, VzErrorCode};
pub use vm::{
    handle::VmHandle,
    machine::VirtualMachine,
    metrics::{ResourceDelta, ResourceUsage},
    network_metrics::{NetworkDelta, NetworkUsage, VmnetInterface},
    state::VmState,
};
pub use vsock::VsockConnection;
