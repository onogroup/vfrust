use crate::vm::state::VmState;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid VM configuration: {0}")]
    InvalidConfiguration(String),

    #[error("invalid device configuration: {0}")]
    InvalidDevice(String),

    #[error("invalid bootloader configuration: {0}")]
    InvalidBootloader(String),

    #[error("VM is in state {current:?}, cannot perform {operation}")]
    InvalidState {
        current: VmState,
        operation: &'static str,
    },

    #[error("Virtualization framework error ({code:?}): {message}")]
    VzError { code: VzErrorCode, message: String },

    #[error("configuration validation failed: {0}")]
    ValidationFailed(String),

    #[error("file not found: {0}")]
    FileNotFound(std::path::PathBuf),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("dispatch queue error: {0}")]
    DispatchError(String),

    #[error("operation timed out")]
    Timeout,

    #[error("rosetta is not available on this system")]
    RosettaUnavailable,

    #[error("macOS platform requires Apple Silicon")]
    RequiresAppleSilicon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VzErrorCode {
    Internal,
    InvalidVirtualMachineConfiguration,
    InvalidVirtualMachineState,
    InvalidVirtualMachineStateTransition,
    InvalidDiskImage,
    VirtualMachineLimitExceeded,
    NetworkError,
    OutOfDiskSpace,
    OperationCancelled,
    NotSupported,
    Save,
    Restore,
    NbdNegotiationFailed,
    NbdDisconnected,
    UsbControllerNotFound,
    DeviceAlreadyAttached,
    Unknown(isize),
}

impl VzErrorCode {
    pub fn from_ns_code(code: isize) -> Self {
        match code {
            1 => Self::Internal,
            2 => Self::InvalidVirtualMachineConfiguration,
            3 => Self::InvalidVirtualMachineState,
            4 => Self::InvalidVirtualMachineStateTransition,
            5 => Self::InvalidDiskImage,
            6 => Self::VirtualMachineLimitExceeded,
            7 => Self::NetworkError,
            8 => Self::OutOfDiskSpace,
            9 => Self::OperationCancelled,
            10 => Self::NotSupported,
            11 => Self::Save,
            12 => Self::Restore,
            20001 => Self::NbdNegotiationFailed,
            20002 => Self::NbdDisconnected,
            30001 => Self::UsbControllerNotFound,
            30002 => Self::DeviceAlreadyAttached,
            other => Self::Unknown(other),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
