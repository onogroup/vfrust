use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioVsock {
    pub port: u32,
    pub socket_url: Option<String>,
    pub listen: bool,
}
