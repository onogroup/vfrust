use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioGpu {
    pub width: u32,
    pub height: u32,
}

impl Default for VirtioGpu {
    fn default() -> Self {
        Self {
            width: 800,
            height: 600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacGraphics {
    pub width: u32,
    pub height: u32,
    pub pixels_per_inch: u32,
}

impl Default for MacGraphics {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1200,
            pixels_per_inch: 144,
        }
    }
}
