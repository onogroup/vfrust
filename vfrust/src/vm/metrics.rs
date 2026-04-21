//! Host-observed resource usage for a running VM.
//!
//! Apple's Virtualization.framework exposes no per-VM runtime counters at
//! the `VZVirtualMachine` layer. These metrics come from
//! `proc_pid_rusage(RUSAGE_INFO_V4)` on the
//! `com.apple.Virtualization.VirtualMachine` subprocess that the framework
//! spawns per VM — the same source that backs Activity Monitor's per-VM
//! row.
//!
//! The values are **host-observed**, not guest-internal:
//!
//! - CPU time is real host time the hypervisor consumed, not guest-observed
//!   CPU.
//! - Memory is the worker process's host backing footprint — approximately
//!   guest-allocated-physical minus pages returned via the balloon. It is
//!   not guest-free memory.
//! - Disk I/O is bytes the worker read/wrote to the host filesystem across
//!   all disk-image attachments, aggregated — no per-disk breakdown.
//! - Network counters are **not** reported; Apple does not expose them at
//!   the framework layer.
//!
//! Energy and CPU perf counters (`energy_nj`, `instructions`, `cycles`) are
//! Apple-Silicon-only; they are `0` on Intel Macs.
//!
//! # Example
//!
//! ```no_run
//! # async fn demo(config: vfrust::VmConfig) -> Result<(), Box<dyn std::error::Error>> {
//! let vm = vfrust::VirtualMachine::new(config)?;
//! let handle = vm.handle();
//! handle.start().await?;
//!
//! if let Some(usage) = handle.resource_usage() {
//!     println!(
//!         "cpu: {:.2}s, memory: {} MiB",
//!         (usage.cpu_user_ns + usage.cpu_system_ns) as f64 / 1e9,
//!         usage.phys_footprint_bytes / (1024 * 1024)
//!     );
//! }
//! # Ok(()) }
//! ```

use std::fmt;
use std::time::{Duration, SystemTime};

/// Host-observed resource usage of the VZ worker process backing a VM.
///
/// See the [module-level documentation](self) for the full list of caveats.
///
/// The struct is `#[non_exhaustive]` so additional counters can be added in
/// the future without a SemVer break.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ResourceUsage {
    /// When this sample was taken (wall-clock time, `SystemTime::now()`).
    pub sampled_at: SystemTime,
    /// Cumulative user-space CPU time, in nanoseconds.
    pub cpu_user_ns: u64,
    /// Cumulative kernel-space CPU time, in nanoseconds.
    pub cpu_system_ns: u64,
    /// Current resident-set size (physical memory mapped in), in bytes.
    pub resident_bytes: u64,
    /// Current physical memory footprint, in bytes. Includes compressed
    /// memory; this is the value Activity Monitor reports as "Memory".
    pub phys_footprint_bytes: u64,
    /// Peak physical footprint over the process's lifetime, in bytes.
    /// Sourced from `ri_interval_max_phys_footprint`.
    pub peak_phys_footprint_bytes: u64,
    /// Current wired (non-pageable) memory, in bytes.
    pub wired_bytes: u64,
    /// Cumulative bytes read from disk by the worker process.
    pub disk_read_bytes: u64,
    /// Cumulative bytes written to disk by the worker process.
    pub disk_write_bytes: u64,
    /// Cumulative page-ins (number of soft/hard page faults serviced).
    pub pageins: u64,
    /// Cumulative billed energy in nanojoules. Apple-Silicon only; `0`
    /// on Intel Macs.
    pub energy_nj: u64,
    /// Cumulative retired instructions. Apple-Silicon only; `0` on Intel.
    pub instructions: u64,
    /// Cumulative CPU cycles. Apple-Silicon only; `0` on Intel.
    pub cycles: u64,
}

/// Per-interval differences between two [`ResourceUsage`] samples.
///
/// Cumulative counters become per-interval totals; snapshot values
/// (`resident_bytes`, `phys_footprint_bytes`, `peak_phys_footprint_bytes`,
/// `wired_bytes`) carry the *later* sample's value unchanged.
///
/// The struct is `#[non_exhaustive]` to allow additional fields without a
/// SemVer break.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ResourceDelta {
    /// Wall-clock time between the two samples.
    pub interval: Duration,
    pub cpu_user_ns: u64,
    pub cpu_system_ns: u64,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub pageins: u64,
    pub energy_nj: u64,
    pub instructions: u64,
    pub cycles: u64,
    /// Later sample's value, verbatim.
    pub resident_bytes: u64,
    /// Later sample's value, verbatim.
    pub phys_footprint_bytes: u64,
    /// Later sample's value, verbatim.
    pub peak_phys_footprint_bytes: u64,
    /// Later sample's value, verbatim.
    pub wired_bytes: u64,
}

impl ResourceUsage {
    /// Compute per-interval deltas against an earlier sample.
    ///
    /// Returns `None` if `prev` was sampled after `self` (caller swapped
    /// the arguments, or wall-clock ran backwards between samples).
    /// Cumulative counters saturate at `0` on the (unexpected) case where
    /// an individual counter decreased — e.g. if the kernel reset its
    /// internal accounting or the worker was restarted between samples.
    pub fn delta_since(&self, prev: &Self) -> Option<ResourceDelta> {
        let interval = self.sampled_at.duration_since(prev.sampled_at).ok()?;
        Some(ResourceDelta {
            interval,
            cpu_user_ns: self.cpu_user_ns.saturating_sub(prev.cpu_user_ns),
            cpu_system_ns: self.cpu_system_ns.saturating_sub(prev.cpu_system_ns),
            disk_read_bytes: self.disk_read_bytes.saturating_sub(prev.disk_read_bytes),
            disk_write_bytes: self.disk_write_bytes.saturating_sub(prev.disk_write_bytes),
            pageins: self.pageins.saturating_sub(prev.pageins),
            energy_nj: self.energy_nj.saturating_sub(prev.energy_nj),
            instructions: self.instructions.saturating_sub(prev.instructions),
            cycles: self.cycles.saturating_sub(prev.cycles),
            resident_bytes: self.resident_bytes,
            phys_footprint_bytes: self.phys_footprint_bytes,
            peak_phys_footprint_bytes: self.peak_phys_footprint_bytes,
            wired_bytes: self.wired_bytes,
        })
    }
}

fn fmt_mib(bytes: u64) -> String {
    format!("{}MiB", bytes / (1024 * 1024))
}

impl fmt::Display for ResourceUsage {
    /// One-liner summary suitable for log output, e.g.
    /// `cpu=1.23s mem=512MiB disk=r:12MiB/w:4MiB`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cpu_s = (self.cpu_user_ns + self.cpu_system_ns) as f64 / 1e9;
        write!(
            f,
            "cpu={:.2}s mem={} disk=r:{}/w:{}",
            cpu_s,
            fmt_mib(self.phys_footprint_bytes),
            fmt_mib(self.disk_read_bytes),
            fmt_mib(self.disk_write_bytes),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t_offset_ms: u64, cpu_u: u64, cpu_s: u64, disk_w: u64) -> ResourceUsage {
        ResourceUsage {
            sampled_at: SystemTime::UNIX_EPOCH + Duration::from_millis(t_offset_ms),
            cpu_user_ns: cpu_u,
            cpu_system_ns: cpu_s,
            resident_bytes: 100,
            phys_footprint_bytes: 200,
            peak_phys_footprint_bytes: 300,
            wired_bytes: 10,
            disk_read_bytes: 0,
            disk_write_bytes: disk_w,
            pageins: 0,
            energy_nj: 0,
            instructions: 0,
            cycles: 0,
        }
    }

    #[test]
    fn delta_since_returns_none_when_prev_is_later() {
        let earlier = sample(0, 0, 0, 0);
        let later = sample(1_000, 100, 50, 10);
        // Swapped: prev > self.
        assert!(later.delta_since(&later).is_some()); // zero interval is fine
        assert!(earlier.delta_since(&later).is_none());
    }

    #[test]
    fn delta_since_subtracts_cumulative_counters() {
        let a = sample(0, 100, 50, 1_000);
        let b = sample(1_000, 400, 150, 5_000);
        let d = b.delta_since(&a).expect("delta");
        assert_eq!(d.interval, Duration::from_millis(1_000));
        assert_eq!(d.cpu_user_ns, 300);
        assert_eq!(d.cpu_system_ns, 100);
        assert_eq!(d.disk_write_bytes, 4_000);
    }

    #[test]
    fn delta_since_saturates_on_decrease() {
        let a = sample(0, 500, 0, 0);
        let b = sample(1_000, 100, 0, 0);
        let d = b.delta_since(&a).expect("delta");
        assert_eq!(d.cpu_user_ns, 0);
    }

    #[test]
    fn delta_since_carries_latter_snapshot_values() {
        let mut a = sample(0, 0, 0, 0);
        a.phys_footprint_bytes = 100;
        let mut b = sample(1_000, 0, 0, 0);
        b.phys_footprint_bytes = 500;
        let d = b.delta_since(&a).expect("delta");
        assert_eq!(d.phys_footprint_bytes, 500);
    }

    #[test]
    fn display_format_matches_expected_shape() {
        let u = ResourceUsage {
            sampled_at: SystemTime::UNIX_EPOCH,
            cpu_user_ns: 1_000_000_000,
            cpu_system_ns: 230_000_000,
            resident_bytes: 0,
            phys_footprint_bytes: 512 * 1024 * 1024,
            peak_phys_footprint_bytes: 0,
            wired_bytes: 0,
            disk_read_bytes: 12 * 1024 * 1024,
            disk_write_bytes: 4 * 1024 * 1024,
            pageins: 0,
            energy_nj: 0,
            instructions: 0,
            cycles: 0,
        };
        assert_eq!(format!("{u}"), "cpu=1.23s mem=512MiB disk=r:12MiB/w:4MiB");
    }
}
