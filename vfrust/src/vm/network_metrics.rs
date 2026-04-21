//! Per-NIC byte / packet counters for `NetAttachment::Vmnet` devices.
//!
//! These are **userspace-observed** counters: every packet traverses
//! vfrust's vmnet ↔ `socketpair` proxy ([`crate::vm::vmnet_proxy`]), so
//! the counts reflect what actually crossed the wire between the guest
//! and the host network stack.
//!
//! Network counters are only reported for `Vmnet` attachments. `Nat`,
//! `UnixSocket`, and `FileDescriptor` attachments do not go through our
//! proxy and are not counted — for those variants, callers own the fds
//! and can count bytes themselves.
//!
//! # Example
//!
//! ```no_run
//! # async fn demo(config: vfrust::VmConfig) -> Result<(), Box<dyn std::error::Error>> {
//! let vm = vfrust::VirtualMachine::new(config)?;
//! let handle = vm.handle();
//! handle.start().await?;
//!
//! let before = handle.network_usage();
//! # tokio::time::sleep(std::time::Duration::from_secs(1)).await;
//! let after = handle.network_usage();
//! for (nic_idx, (a, b)) in before.iter().zip(after.iter()).enumerate() {
//!     if let Some(delta) = b.delta_since(a) {
//!         println!("nic {nic_idx}: {delta}");
//!     }
//! }
//! # Ok(()) }
//! ```

use std::fmt;
use std::net::Ipv4Addr;
use std::time::{Duration, SystemTime};

use crate::config::device::network::MacAddress;

/// Userspace-observed per-NIC byte / packet counters for a single
/// `NetAttachment::Vmnet` NIC.
///
/// The struct is `#[non_exhaustive]` so additional counters can be added
/// in the future without a SemVer break.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct NetworkUsage {
    /// When this sample was taken (wall-clock time, `SystemTime::now()`).
    pub sampled_at: SystemTime,
    /// Cumulative bytes received by the guest from the host network.
    pub rx_bytes: u64,
    /// Cumulative bytes sent by the guest toward the host network.
    pub tx_bytes: u64,
    /// Cumulative packets received.
    pub rx_packets: u64,
    /// Cumulative packets sent.
    pub tx_packets: u64,
    /// Cumulative receive-side drops observed by the proxy.
    /// Elevated `rx_drops` typically indicates the guest kernel is not
    /// consuming packets fast enough.
    pub rx_drops: u64,
    /// Cumulative transmit-side drops observed by the proxy.
    /// Elevated `tx_drops` typically indicates vmnet rejected writes
    /// (e.g. congestion on the host bridge).
    pub tx_drops: u64,
}

/// Per-interval difference between two [`NetworkUsage`] samples.
///
/// Cumulative counters become per-interval totals. The struct is
/// `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct NetworkDelta {
    /// Wall-clock time between the two samples.
    pub interval: Duration,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_drops: u64,
    pub tx_drops: u64,
}

impl NetworkUsage {
    /// Compute per-interval deltas against an earlier sample.
    ///
    /// Returns `None` if `prev` was sampled after `self` (caller swapped
    /// the arguments, or wall-clock ran backwards between samples).
    /// Cumulative counters saturate at `0` on the (unexpected) case where
    /// an individual counter decreased — e.g. if the proxy was restarted
    /// between samples.
    pub fn delta_since(&self, prev: &Self) -> Option<NetworkDelta> {
        let interval = self.sampled_at.duration_since(prev.sampled_at).ok()?;
        Some(NetworkDelta {
            interval,
            rx_bytes: self.rx_bytes.saturating_sub(prev.rx_bytes),
            tx_bytes: self.tx_bytes.saturating_sub(prev.tx_bytes),
            rx_packets: self.rx_packets.saturating_sub(prev.rx_packets),
            tx_packets: self.tx_packets.saturating_sub(prev.tx_packets),
            rx_drops: self.rx_drops.saturating_sub(prev.rx_drops),
            tx_drops: self.tx_drops.saturating_sub(prev.tx_drops),
        })
    }
}

fn fmt_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1}GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{}MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{}KiB", bytes / KIB)
    } else {
        format!("{bytes}B")
    }
}

impl fmt::Display for NetworkUsage {
    /// One-liner suitable for log output, e.g.
    /// `rx=12MiB/458pkt tx=3MiB/127pkt`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rx={}/{}pkt tx={}/{}pkt",
            fmt_bytes(self.rx_bytes),
            self.rx_packets,
            fmt_bytes(self.tx_bytes),
            self.tx_packets,
        )
    }
}

impl fmt::Display for NetworkDelta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rx={}/{}pkt tx={}/{}pkt",
            fmt_bytes(self.rx_bytes),
            self.rx_packets,
            fmt_bytes(self.tx_bytes),
            self.tx_packets,
        )
    }
}

/// Fixed metadata about a running `NetAttachment::Vmnet` interface —
/// the vmnet-assigned MAC, MTU, and DHCP pool as returned by
/// `vmnet_start_interface`.
///
/// Useful for tests and diagnostics (e.g. resolving the guest IP from
/// the host ARP cache once DHCP is done). One entry per Vmnet NIC, in
/// the same order as `network_usage()`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VmnetInterface {
    pub mac: MacAddress,
    pub mtu: u32,
    pub max_packet_size: u32,
    pub dhcp_start: Option<Ipv4Addr>,
    pub dhcp_end: Option<Ipv4Addr>,
    pub subnet_mask: Option<Ipv4Addr>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t_offset_ms: u64, rx_b: u64, tx_b: u64, rx_p: u64, tx_p: u64) -> NetworkUsage {
        NetworkUsage {
            sampled_at: SystemTime::UNIX_EPOCH + Duration::from_millis(t_offset_ms),
            rx_bytes: rx_b,
            tx_bytes: tx_b,
            rx_packets: rx_p,
            tx_packets: tx_p,
            rx_drops: 0,
            tx_drops: 0,
        }
    }

    #[test]
    fn delta_since_returns_none_when_prev_is_later() {
        let earlier = sample(0, 0, 0, 0, 0);
        let later = sample(1_000, 10, 10, 1, 1);
        assert!(later.delta_since(&later).is_some());
        assert!(earlier.delta_since(&later).is_none());
    }

    #[test]
    fn delta_since_subtracts_cumulative_counters() {
        let a = sample(0, 100, 50, 10, 5);
        let b = sample(1_000, 400, 150, 40, 15);
        let d = b.delta_since(&a).expect("delta");
        assert_eq!(d.interval, Duration::from_millis(1_000));
        assert_eq!(d.rx_bytes, 300);
        assert_eq!(d.tx_bytes, 100);
        assert_eq!(d.rx_packets, 30);
        assert_eq!(d.tx_packets, 10);
    }

    #[test]
    fn delta_since_saturates_on_decrease() {
        let a = sample(0, 500, 500, 5, 5);
        let b = sample(1_000, 100, 100, 1, 1);
        let d = b.delta_since(&a).expect("delta");
        assert_eq!(d.rx_bytes, 0);
        assert_eq!(d.tx_bytes, 0);
        assert_eq!(d.rx_packets, 0);
    }

    #[test]
    fn display_format_uses_human_sizes() {
        let u = NetworkUsage {
            sampled_at: SystemTime::UNIX_EPOCH,
            rx_bytes: 12 * 1024 * 1024,
            tx_bytes: 3 * 1024 * 1024,
            rx_packets: 458,
            tx_packets: 127,
            rx_drops: 0,
            tx_drops: 0,
        };
        assert_eq!(format!("{u}"), "rx=12MiB/458pkt tx=3MiB/127pkt");
    }

    #[test]
    fn display_format_scales_down_to_bytes() {
        let u = NetworkUsage {
            sampled_at: SystemTime::UNIX_EPOCH,
            rx_bytes: 512,
            tx_bytes: 0,
            rx_packets: 1,
            tx_packets: 0,
            rx_drops: 0,
            tx_drops: 0,
        };
        assert_eq!(format!("{u}"), "rx=512B/1pkt tx=0B/0pkt");
    }
}
