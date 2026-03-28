use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioSerial {
    pub attachment: SerialAttachment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SerialAttachment {
    File { path: PathBuf },
    Stdio,
    Pty,
}
