//! Runtime capability probe for `NetAttachment::Vmnet`.
//!
//! `com.apple.vm.networking` is a restricted entitlement on macOS 26+:
//! ad-hoc-signed binaries that declare it are killed by AMFI at launch.
//! Binaries signed with a Developer ID provisioning profile, or processes
//! running as root, are allowed. The only robust way to know whether a
//! given build + launch context can actually use vmnet is to try starting
//! a throwaway interface and inspect the return code.
//!
//! Callers that want to offer vmnet conditionally (fall back to
//! `NetAttachment::Nat` when vmnet is denied) should call
//! [`probe_vmnet`] once at startup and cache the result.

use crate::config::device::network::{VmnetConfig, VmnetMode};
pub use crate::sys::vmnet::VmnetReturn;
use crate::sys::vmnet::{start_interface, stop_interface};

/// Outcome of [`probe_vmnet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VmnetProbe {
    /// `vmnet_start_interface` succeeded. `NetAttachment::Vmnet` will work
    /// in this process.
    Available,
    /// vmnet refused with `VMNET_INVALID_ACCESS` (code 1005). The process
    /// is missing the `com.apple.vm.networking` entitlement, is not
    /// running as root, or the binary's signature is not accepted by AMFI
    /// for this entitlement (ad-hoc on macOS 26+). Fall back to
    /// `NetAttachment::Nat` or re-sign with a Developer ID profile.
    Denied,
    /// vmnet failed for some other reason (transient `SHARING_SERVICE_BUSY`,
    /// `vmnet.framework` unavailable, etc). The embedded `VmnetReturn` is
    /// preserved for diagnostics via the `Display` impl.
    Unavailable(VmnetReturn),
}

impl std::fmt::Display for VmnetProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available => f.write_str("vmnet available"),
            Self::Denied => f.write_str(
                "vmnet denied: run as root or codesign with a Developer ID \
                 profile that includes com.apple.vm.networking",
            ),
            Self::Unavailable(code) => write!(f, "vmnet unavailable: {code}"),
        }
    }
}

/// Probe whether `NetAttachment::Vmnet` is usable in this process.
///
/// Starts a throwaway Shared-mode vmnet interface with Apple's default
/// DHCP range, immediately stops it, and classifies the outcome. The
/// probe takes ~100ms on success and is safe to call before any VM is
/// started.
///
/// Emits a `tracing::warn!` on `Denied` with an actionable message so
/// production deployments surface the misconfiguration even if the
/// return value is ignored.
pub fn probe_vmnet() -> VmnetProbe {
    let cfg = VmnetConfig {
        mode: VmnetMode::Shared,
        ..Default::default()
    };
    match start_interface(&cfg) {
        Ok((handle, _params)) => {
            // Best-effort teardown — if stop fails we still report
            // Available since start succeeded.
            let _ = stop_interface(handle);
            VmnetProbe::Available
        }
        Err(e) if matches!(e.code, VmnetReturn::InvalidAccess) => {
            tracing::warn!(
                "vmnet denied (VMNET_INVALID_ACCESS): binary is missing \
                 com.apple.vm.networking entitlement, is ad-hoc signed on \
                 macOS 26+, or is not running as root. Re-sign with a \
                 Developer ID provisioning profile or run as root to use \
                 NetAttachment::Vmnet; otherwise fall back to \
                 NetAttachment::Nat."
            );
            VmnetProbe::Denied
        }
        Err(e) => VmnetProbe::Unavailable(e.code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_vmnet_returns_a_variant_without_panicking() {
        // Don't assert a specific outcome — it depends on entitlement
        // and uid at test time. We only care that the probe runs to
        // completion and returns *something*.
        let out = probe_vmnet();
        let s = format!("{out}");
        assert!(!s.is_empty());
    }
}
