use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioSound {
    pub input: bool,
    pub output: bool,
}

impl Default for VirtioSound {
    fn default() -> Self {
        Self {
            input: false,
            output: true,
        }
    }
}
