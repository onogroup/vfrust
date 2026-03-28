use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum Platform {
    #[default]
    Generic,
    MacOs(MacOsPlatform),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacOsPlatform {
    pub machine_identifier_path: PathBuf,
    pub hardware_model_path: PathBuf,
    pub aux_storage_path: PathBuf,
}
