use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiskBackend {
    #[default]
    Image,
    BlockDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiskCachingMode {
    #[default]
    Automatic,
    Cached,
    Uncached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiskSyncMode {
    #[default]
    Full,
    Fsync,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioBlk {
    pub path: PathBuf,
    pub read_only: bool,
    pub backend: DiskBackend,
    pub device_id: Option<String>,
    pub caching_mode: DiskCachingMode,
    pub sync_mode: DiskSyncMode,
}

impl Default for VirtioBlk {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            read_only: false,
            backend: DiskBackend::default(),
            device_id: None,
            caching_mode: DiskCachingMode::default(),
            sync_mode: DiskSyncMode::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nvme {
    pub path: PathBuf,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsbMassStorage {
    pub path: PathBuf,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nbd {
    pub uri: String,
    pub device_id: Option<String>,
    pub timeout: Option<std::time::Duration>,
    pub sync_mode: DiskSyncMode,
    pub read_only: bool,
}
