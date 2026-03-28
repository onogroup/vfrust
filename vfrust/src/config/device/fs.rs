use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedDir {
    pub name: String,
    pub path: PathBuf,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioFs {
    pub mount_tag: String,
    pub shared_dir: Option<PathBuf>,
    pub directories: Vec<SharedDir>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rosetta {
    pub mount_tag: String,
    pub install: bool,
    pub ignore_if_missing: bool,
}
