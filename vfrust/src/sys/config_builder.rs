use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_virtualization::*;

use crate::config::bootloader::Bootloader;
use crate::config::platform::Platform;
use crate::config::vm::VmConfig;
use crate::sys::bootloader::build_bootloader;
use crate::sys::device::build_devices;

/// Output of `build_vz_config`: the VZ configuration plus any live
/// side-effect resources (currently `VmnetProxy` packet pumps) that must
/// be kept alive on `InnerMachine` for the VM's lifetime.
pub(crate) struct BuiltVzConfig {
    pub vz_config: Retained<VZVirtualMachineConfiguration>,
    pub network_proxies: Vec<std::sync::Arc<crate::vm::vmnet_proxy::VmnetProxy>>,
}

pub(crate) fn build_vz_config(config: &VmConfig) -> crate::Result<BuiltVzConfig> {
    // Validate: nested virtualization cannot be used with macOS bootloader.
    if config.nested && matches!(config.bootloader, Bootloader::MacOs(_)) {
        return Err(crate::Error::InvalidConfiguration(
            "nested virtualization is not supported with macOS bootloader".into(),
        ));
    }

    unsafe {
        let vz_config = VZVirtualMachineConfiguration::new();

        vz_config.setCPUCount(config.cpus as usize);
        vz_config.setMemorySize(config.memory_mib * 1024 * 1024);

        // Platform
        let platform = build_platform(&config.platform, config.nested, config.machine_identifier.as_deref())?;
        vz_config.setPlatform(&platform);

        // Bootloader
        let bootloader = build_bootloader(&config.bootloader)?;
        vz_config.setBootLoader(Some(&bootloader));

        // Devices
        let built = build_devices(&config.devices)?;
        vz_config.setStorageDevices(&built.storage);
        vz_config.setNetworkDevices(&built.network);
        vz_config.setSerialPorts(&built.serial);
        vz_config.setSocketDevices(&built.socket);
        vz_config.setEntropyDevices(&built.entropy);
        vz_config.setMemoryBalloonDevices(&built.balloon);
        vz_config.setGraphicsDevices(&built.graphics);
        vz_config.setKeyboards(&built.keyboard);
        vz_config.setPointingDevices(&built.pointing);
        vz_config.setDirectorySharingDevices(&built.dir_sharing);
        vz_config.setAudioDevices(&built.audio);
        vz_config.setUsbControllers(&built.usb_controllers);

        // Validate
        vz_config
            .validateWithError()
            .map_err(|e| crate::Error::ValidationFailed(e.to_string()))?;

        // Check if save/restore is supported.  This is advisory — many valid
        // configurations (e.g. with GPU or USB pass-through) do not support it.
        // We log the result rather than failing so callers can still create
        // the VM; save/restore will return an error at call time if unsupported.
        match vz_config.validateSaveRestoreSupportWithError() {
            Ok(()) => {
                tracing::debug!("VM configuration supports save/restore");
            }
            Err(e) => {
                tracing::warn!("VM configuration does NOT support save/restore: {e}");
            }
        }

        Ok(BuiltVzConfig {
            vz_config,
            network_proxies: built.network_proxies,
        })
    }
}

fn build_platform(
    platform: &Platform,
    nested: bool,
    machine_identifier: Option<&[u8]>,
) -> crate::Result<Retained<VZPlatformConfiguration>> {
    unsafe {
        match platform {
            Platform::Generic => {
                let config = VZGenericPlatformConfiguration::new();

                // Set machine identifier if provided (required for save/restore).
                if let Some(id_bytes) = machine_identifier {
                    let ns_data = objc2_foundation::NSData::with_bytes(id_bytes);
                    let machine_id = VZGenericMachineIdentifier::initWithDataRepresentation(
                        VZGenericMachineIdentifier::alloc(),
                        &ns_data,
                    )
                    .ok_or_else(|| crate::Error::InvalidConfiguration(
                        "invalid generic machine identifier data".into(),
                    ))?;
                    config.setMachineIdentifier(&machine_id);
                }

                if nested {
                    if VZGenericPlatformConfiguration::isNestedVirtualizationSupported() {
                        config.setNestedVirtualizationEnabled(true);
                        tracing::info!("nested virtualization enabled");
                    } else {
                        return Err(crate::Error::InvalidConfiguration(
                            "nested virtualization is not supported on this system".into(),
                        ));
                    }
                }

                Ok(Retained::into_super(config))
            }
            Platform::MacOs(mac) => {
                let config = VZMacPlatformConfiguration::new();

                // Load hardware model from file
                let hw_data = std::fs::read(&mac.hardware_model_path)?;
                let ns_data = objc2_foundation::NSData::with_bytes(&hw_data);
                if let Some(hw_model) = VZMacHardwareModel::initWithDataRepresentation(
                    VZMacHardwareModel::alloc(),
                    &ns_data,
                ) {
                    config.setHardwareModel(&hw_model);
                } else {
                    return Err(crate::Error::InvalidConfiguration(
                        "invalid hardware model data".into(),
                    ));
                }

                // Load machine identifier from file
                let id_data = std::fs::read(&mac.machine_identifier_path)?;
                let ns_id_data = objc2_foundation::NSData::with_bytes(&id_data);
                if let Some(machine_id) = VZMacMachineIdentifier::initWithDataRepresentation(
                    VZMacMachineIdentifier::alloc(),
                    &ns_id_data,
                ) {
                    config.setMachineIdentifier(&machine_id);
                } else {
                    return Err(crate::Error::InvalidConfiguration(
                        "invalid machine identifier data".into(),
                    ));
                }

                // Auxiliary storage
                let aux_url = crate::sys::nsurl_from_path(&mac.aux_storage_path)?;
                let aux =
                    VZMacAuxiliaryStorage::initWithURL(VZMacAuxiliaryStorage::alloc(), &aux_url);
                config.setAuxiliaryStorage(Some(&aux));

                Ok(Retained::into_super(config))
            }
        }
    }
}
