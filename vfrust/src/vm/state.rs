use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VmState {
    Stopped,
    Running,
    Paused,
    Error,
    Starting,
    Pausing,
    Resuming,
    Stopping,
    Saving,
    Restoring,
}

impl VmState {
    pub fn can_start(&self) -> bool {
        *self == VmState::Stopped
    }

    pub fn can_pause(&self) -> bool {
        *self == VmState::Running
    }

    pub fn can_resume(&self) -> bool {
        *self == VmState::Paused
    }

    pub fn can_stop(&self) -> bool {
        matches!(self, VmState::Running | VmState::Paused | VmState::Error)
    }

    pub fn can_request_stop(&self) -> bool {
        *self == VmState::Running
    }
}

impl std::fmt::Display for VmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmState::Stopped => write!(f, "Stopped"),
            VmState::Running => write!(f, "Running"),
            VmState::Paused => write!(f, "Paused"),
            VmState::Error => write!(f, "Error"),
            VmState::Starting => write!(f, "Starting"),
            VmState::Pausing => write!(f, "Pausing"),
            VmState::Resuming => write!(f, "Resuming"),
            VmState::Stopping => write!(f, "Stopping"),
            VmState::Saving => write!(f, "Saving"),
            VmState::Restoring => write!(f, "Restoring"),
        }
    }
}
