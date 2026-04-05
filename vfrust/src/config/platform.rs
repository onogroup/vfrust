use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum Platform {
    /// Generic platform (Linux/ARM64 guests).
    ///
    /// Use [`VmBuilder::machine_identifier`] to supply an existing identity
    /// (required for save/restore — the save file is bound to it).
    /// If no identifier is provided, Apple VZ auto-generates a new one.
    ///
    /// Retrieve the resolved config (including auto-generated identifier and
    /// MACs) via [`VirtualMachine::snapshot_config`] / [`VmHandle::snapshot_config`]
    /// and persist it for future restore calls.
    #[default]
    Generic,
    MacOs(MacOsPlatform),
}

/// Opaque machine identifier bytes (`VZGenericMachineIdentifier.dataRepresentation`).
///
/// Obtain via [`VirtualMachine::snapshot_config`] /
/// [`VmHandle::snapshot_config`] and persist alongside the VM's save file.
/// Supply it via [`VmBuilder::machine_identifier`] when creating the restore
/// VM, or simply load the entire snapshot config from JSON.
pub type MachineIdentifier = Vec<u8>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacOsPlatform {
    pub machine_identifier_path: PathBuf,
    pub hardware_model_path: PathBuf,
    pub aux_storage_path: PathBuf,
}
