use serde::{Deserialize, Serialize};

use super::bootloader::Bootloader;
use super::device::Device;
use super::platform::{MachineIdentifier, Platform};

/// Complete, validated VM configuration. Immutable once built.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub(crate) cpus: u32,
    pub(crate) memory_mib: u64,
    pub(crate) bootloader: Bootloader,
    pub(crate) platform: Platform,
    pub(crate) devices: Vec<Device>,
    pub(crate) nested: bool,
    /// Optional machine identifier for the Generic platform (ignored for macOS).
    ///
    /// When `Some`, the VM is created with this identity (required for
    /// save/restore — the save file is bound to the identifier).
    /// When `None`, Virtualization.framework auto-generates a new identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) machine_identifier: Option<MachineIdentifier>,
}

impl VmConfig {
    pub fn builder() -> VmBuilder {
        VmBuilder::default()
    }

    pub fn cpus(&self) -> u32 {
        self.cpus
    }

    pub fn memory_mib(&self) -> u64 {
        self.memory_mib
    }

    pub fn bootloader(&self) -> &Bootloader {
        &self.bootloader
    }

    pub fn platform(&self) -> &Platform {
        &self.platform
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    pub fn nested(&self) -> bool {
        self.nested
    }

    /// The machine identifier for the Generic platform, if present.
    ///
    /// On a config obtained from [`VirtualMachine::snapshot_config`] /
    /// [`VmHandle::snapshot_config`] this is the actual identifier in use
    /// (including auto-generated ones).  On a builder-created config it is
    /// only present when explicitly set via [`VmBuilder::machine_identifier`].
    pub fn machine_identifier(&self) -> Option<&MachineIdentifier> {
        self.machine_identifier.as_ref()
    }

    pub fn from_json(path: &std::path::Path) -> crate::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content).map_err(|e| crate::Error::InvalidConfiguration(e.to_string()))
    }

    pub fn to_json(&self) -> crate::Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| crate::Error::InvalidConfiguration(e.to_string()))
    }
}

#[derive(Debug, Default, Serialize)]
pub struct VmBuilder {
    cpus: Option<u32>,
    memory_mib: Option<u64>,
    bootloader: Option<Bootloader>,
    platform: Platform,
    devices: Vec<Device>,
    nested: bool,
    machine_identifier: Option<MachineIdentifier>,
}

impl VmBuilder {
    pub fn cpus(mut self, count: u32) -> Self {
        self.cpus = Some(count);
        self
    }

    pub fn memory_mib(mut self, mib: u64) -> Self {
        self.memory_mib = Some(mib);
        self
    }

    pub fn bootloader(mut self, bootloader: Bootloader) -> Self {
        self.bootloader = Some(bootloader);
        self
    }

    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    pub fn device(mut self, device: Device) -> Self {
        self.devices.push(device);
        self
    }

    pub fn devices(mut self, devices: impl IntoIterator<Item = Device>) -> Self {
        self.devices.extend(devices);
        self
    }

    pub fn nested(mut self, nested: bool) -> Self {
        self.nested = nested;
        self
    }

    /// Set the machine identifier for the Generic platform.
    ///
    /// Required when restoring from a save file, since the save is bound to
    /// the identifier that was active when `saveMachineState` was called.
    /// Obtain it from [`VirtualMachine::snapshot_config`] or
    /// [`VmHandle::snapshot_config`].
    ///
    /// Ignored when the platform is [`Platform::MacOs`] (macOS uses its own
    /// `VZMacMachineIdentifier` loaded from a file).
    pub fn machine_identifier(mut self, id: MachineIdentifier) -> Self {
        self.machine_identifier = Some(id);
        self
    }

    pub fn build(self) -> crate::Result<VmConfig> {
        let cpus = self.cpus.unwrap_or(1);
        let memory_mib = self.memory_mib.unwrap_or(512);
        let bootloader = self
            .bootloader
            .ok_or_else(|| crate::Error::InvalidConfiguration("bootloader is required".into()))?;

        if cpus == 0 {
            return Err(crate::Error::InvalidConfiguration(
                "cpus must be at least 1".into(),
            ));
        }
        if memory_mib < 128 {
            return Err(crate::Error::InvalidConfiguration(
                "memory must be at least 128 MiB".into(),
            ));
        }

        Ok(VmConfig {
            cpus,
            memory_mib,
            bootloader,
            platform: self.platform,
            devices: self.devices,
            nested: self.nested,
            machine_identifier: self.machine_identifier,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::bootloader::LinuxBootloader;

    #[test]
    fn test_builder_requires_bootloader() {
        let result = VmConfig::builder().build();
        assert!(result.is_err());
    }

    #[test]
    fn test_builder_defaults() {
        let config = VmConfig::builder()
            .bootloader(Bootloader::Linux(LinuxBootloader {
                kernel_path: "/tmp/vmlinuz".into(),
                initrd_path: None,
                command_line: String::new(),
            }))
            .build()
            .unwrap();
        assert_eq!(config.cpus(), 1);
        assert_eq!(config.memory_mib(), 512);
        assert!(!config.nested());
    }

    #[test]
    fn test_builder_rejects_zero_cpus() {
        let result = VmConfig::builder()
            .cpus(0)
            .bootloader(Bootloader::Linux(LinuxBootloader {
                kernel_path: "/tmp/vmlinuz".into(),
                initrd_path: None,
                command_line: String::new(),
            }))
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn test_builder_rejects_low_memory() {
        let result = VmConfig::builder()
            .memory_mib(64)
            .bootloader(Bootloader::Linux(LinuxBootloader {
                kernel_path: "/tmp/vmlinuz".into(),
                initrd_path: None,
                command_line: String::new(),
            }))
            .build();
        assert!(result.is_err());
    }
}
