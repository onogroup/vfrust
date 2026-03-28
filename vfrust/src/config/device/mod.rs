pub mod audio;
pub mod fs;
pub mod gpu;
pub mod input;
pub mod network;
pub mod serial;
pub mod storage;
pub mod vsock;

use audio::VirtioSound;
use fs::{Rosetta, VirtioFs};
use gpu::{MacGraphics, VirtioGpu};
use input::VirtioInput;
use network::VirtioNet;
use serde::{Deserialize, Serialize};
use serial::VirtioSerial;
use storage::{Nbd, Nvme, UsbMassStorage, VirtioBlk};
use vsock::VirtioVsock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Device {
    VirtioBlk(VirtioBlk),
    Nvme(Nvme),
    UsbMassStorage(UsbMassStorage),
    Nbd(Nbd),
    VirtioNet(VirtioNet),
    VirtioSerial(VirtioSerial),
    VirtioVsock(VirtioVsock),
    VirtioGpu(VirtioGpu),
    MacGraphics(MacGraphics),
    VirtioInput(VirtioInput),
    VirtioFs(VirtioFs),
    Rosetta(Rosetta),
    VirtioSound(VirtioSound),
    VirtioRng,
    VirtioBalloon,
    UsbController,
}
