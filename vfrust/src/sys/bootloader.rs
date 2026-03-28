use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_foundation::NSString;
use objc2_virtualization::*;

use crate::config::bootloader::{Bootloader, EfiBootloader, LinuxBootloader};
use crate::sys::nsurl_from_path;

pub(crate) fn build_bootloader(
    bootloader: &Bootloader,
) -> crate::Result<Retained<VZBootLoader>> {
    match bootloader {
        Bootloader::Linux(linux) => build_linux_bootloader(linux),
        Bootloader::Efi(efi) => build_efi_bootloader(efi),
        Bootloader::MacOs(_) => build_macos_bootloader(),
    }
}

/// Validate that a kernel image is uncompressed on ARM64.
///
/// VZLinuxBootLoader requires raw ARM64 Image format kernels.
/// The ARM64 Image header has magic bytes "ARMd" (0x41, 0x52, 0x4d, 0x64)
/// at offset 0x38. EFI-stub kernels (MZ/PE format) should use VZEFIBootLoader.
#[cfg(target_arch = "aarch64")]
fn validate_kernel_uncompressed(path: &std::path::Path) -> crate::Result<()> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 2048];
    let n = file.read(&mut buf)?;
    if n < 0x3C {
        return Err(crate::Error::InvalidBootloader(
            "kernel file too small".into(),
        ));
    }
    let magic = &buf[0x38..0x3C];
    if magic != [0x41, 0x52, 0x4d, 0x64] {
        return Err(crate::Error::InvalidBootloader(
            "kernel must be uncompressed ARM64 Image format (expected ARMd magic at offset 0x38); EFI-stub kernels should use the EFI bootloader".into(),
        ));
    }
    Ok(())
}

fn build_linux_bootloader(linux: &LinuxBootloader) -> crate::Result<Retained<VZBootLoader>> {
    // On ARM64, validate that the kernel image is uncompressed
    #[cfg(target_arch = "aarch64")]
    validate_kernel_uncompressed(&linux.kernel_path)?;

    let kernel_url = nsurl_from_path(&linux.kernel_path)?;
    unsafe {
        let bl = VZLinuxBootLoader::initWithKernelURL(VZLinuxBootLoader::alloc(), &kernel_url);

        if !linux.command_line.is_empty() {
            let cmdline = NSString::from_str(&linux.command_line);
            bl.setCommandLine(&cmdline);
        }

        if let Some(initrd) = &linux.initrd_path {
            let initrd_url = nsurl_from_path(initrd)?;
            bl.setInitialRamdiskURL(Some(&initrd_url));
        }

        Ok(Retained::into_super(bl))
    }
}

fn build_efi_bootloader(efi: &EfiBootloader) -> crate::Result<Retained<VZBootLoader>> {
    let store_url = nsurl_from_path(&efi.variable_store_path)?;
    unsafe {
        let store = if efi.create_variable_store {
            VZEFIVariableStore::initCreatingVariableStoreAtURL_options_error(
                VZEFIVariableStore::alloc(),
                &store_url,
                VZEFIVariableStoreInitializationOptions::empty(),
            )
            .map_err(|e| crate::Error::InvalidBootloader(format!("EFI variable store: {}", e)))?
        } else {
            VZEFIVariableStore::initWithURL(VZEFIVariableStore::alloc(), &store_url)
        };

        let bl = VZEFIBootLoader::new();
        bl.setVariableStore(Some(&store));
        Ok(Retained::into_super(bl))
    }
}

fn build_macos_bootloader() -> crate::Result<Retained<VZBootLoader>> {
    unsafe {
        let bl = VZMacOSBootLoader::new();
        Ok(Retained::into_super(bl))
    }
}
