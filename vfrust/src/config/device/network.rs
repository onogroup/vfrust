use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtioNet {
    pub attachment: NetAttachment,
    pub mac_address: Option<MacAddress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetAttachment {
    Nat,
    UnixSocket { path: PathBuf },
    FileDescriptor { fd: i32 },
    /// Managed networking via Apple's `vmnet.framework`. Unlike `Nat`,
    /// this gives vfrust control over the packet path, which enables
    /// per-interface byte / packet counters via
    /// [`VmHandle::network_usage`](crate::VmHandle::network_usage).
    ///
    /// Requires the `com.apple.vm.networking` entitlement on the host
    /// binary, or running as root. See the crate-level `README` for
    /// entitlement setup.
    Vmnet(VmnetConfig),
}

/// Configuration for a `vmnet.framework`-backed network attachment.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VmnetConfig {
    pub mode: VmnetMode,

    /// Override the DHCP pool start address. `None` → Apple defaults
    /// (Shared mode: `192.168.64.2`). Ignored for Bridged mode.
    pub start_address: Option<Ipv4Addr>,

    /// Override the DHCP pool end address. `None` → Apple defaults
    /// (Shared mode: `192.168.64.254`). Ignored for Bridged mode.
    pub end_address: Option<Ipv4Addr>,

    /// Override the subnet mask. `None` → Apple defaults (`255.255.255.0`).
    /// Ignored for Bridged mode.
    pub subnet_mask: Option<Ipv4Addr>,

    /// If `true`, ask vmnet to assign a MAC. If `false` (default), the
    /// MAC from [`VirtioNet::mac_address`] is used (or a fresh random
    /// locally-administered MAC if that is also `None`).
    pub allocate_mac: bool,

    /// Isolate this VM from other vmnet interfaces on the host bridge.
    /// Maps to `vmnet_enable_isolation_key`.
    pub isolated: bool,

    /// Host physical interface to bridge to. Required when
    /// `mode == VmnetMode::Bridged`, ignored otherwise. Example: `"en0"`.
    pub bridged_interface: Option<String>,
}

/// `vmnet.framework` operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VmnetMode {
    /// Shared NAT — guest sees a private subnet behind the host NAT.
    /// Default pool is `192.168.64.0/24`.
    #[default]
    Shared,
    /// Host-only network — guest can reach the host but not the internet.
    /// Default pool is `192.168.65.0/24`.
    Host,
    /// Bridged to a physical host interface. The guest appears as a peer
    /// on the same L2 segment as the host. Requires
    /// [`VmnetConfig::bridged_interface`] to name a real NIC, and in
    /// practice requires a provisioning-profile-signed binary.
    Bridged,
}

/// A 6-byte MAC address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacAddress(pub [u8; 6]);

impl MacAddress {
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return None;
        }
        let mut bytes = [0u8; 6];
        for (i, part) in parts.iter().enumerate() {
            bytes[i] = u8::from_str_radix(part, 16).ok()?;
        }
        Some(Self(bytes))
    }
}

impl std::fmt::Display for MacAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmnet_config_default_is_shared() {
        let cfg = VmnetConfig::default();
        assert_eq!(cfg.mode, VmnetMode::Shared);
        assert!(cfg.start_address.is_none());
        assert!(!cfg.allocate_mac);
        assert!(!cfg.isolated);
        assert!(cfg.bridged_interface.is_none());
    }

    #[test]
    fn vmnet_mode_serde_uses_snake_case() {
        let shared = serde_json::to_string(&VmnetMode::Shared).unwrap();
        assert_eq!(shared, "\"shared\"");
        let host = serde_json::to_string(&VmnetMode::Host).unwrap();
        assert_eq!(host, "\"host\"");
        let bridged = serde_json::to_string(&VmnetMode::Bridged).unwrap();
        assert_eq!(bridged, "\"bridged\"");
    }

    #[test]
    fn net_attachment_vmnet_round_trips_through_json() {
        let a = NetAttachment::Vmnet(VmnetConfig {
            mode: VmnetMode::Host,
            start_address: Some(Ipv4Addr::new(10, 0, 1, 2)),
            end_address: Some(Ipv4Addr::new(10, 0, 1, 100)),
            subnet_mask: Some(Ipv4Addr::new(255, 255, 255, 0)),
            allocate_mac: true,
            isolated: true,
            bridged_interface: None,
        });
        let json = serde_json::to_string(&a).unwrap();
        let back: NetAttachment = serde_json::from_str(&json).unwrap();
        match back {
            NetAttachment::Vmnet(cfg) => {
                assert_eq!(cfg.mode, VmnetMode::Host);
                assert_eq!(cfg.start_address, Some(Ipv4Addr::new(10, 0, 1, 2)));
                assert_eq!(cfg.end_address, Some(Ipv4Addr::new(10, 0, 1, 100)));
                assert!(cfg.allocate_mac);
                assert!(cfg.isolated);
            }
            other => panic!("expected Vmnet, got {other:?}"),
        }
    }

    #[test]
    fn net_attachment_existing_variants_still_round_trip() {
        for variant in [
            NetAttachment::Nat,
            NetAttachment::FileDescriptor { fd: 42 },
            NetAttachment::UnixSocket {
                path: "/tmp/x.sock".into(),
            },
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let _back: NetAttachment = serde_json::from_str(&json).unwrap();
        }
    }
}
