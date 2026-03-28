use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Bootloader {
    Linux(LinuxBootloader),
    Efi(EfiBootloader),
    MacOs(MacOsBootloader),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinuxBootloader {
    pub kernel_path: PathBuf,
    pub initrd_path: Option<PathBuf>,
    pub command_line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EfiBootloader {
    pub variable_store_path: PathBuf,
    pub create_variable_store: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacOsBootloader {
    pub machine_identifier_path: PathBuf,
    pub hardware_model_path: PathBuf,
    pub aux_image_path: PathBuf,
}
