//! FFI helpers over Darwin `proc_*` APIs used by VM metrics.
//!
//! The public metrics API in [`crate::vm::metrics`] reports host-observed
//! counters by reading `proc_pid_rusage` on the
//! `com.apple.Virtualization.VirtualMachine` worker process that
//! Virtualization.framework spawns per VM.

use std::collections::HashSet;
use std::ffi::c_int;
use std::path::PathBuf;

use libc::pid_t;

/// `proc_pidpath` output buffer size — same as `PROC_PIDPATHINFO_MAXSIZE`.
const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * libc::PATH_MAX as usize;

/// `proc_name` output buffer size — generous; `p_comm` is ~16 bytes but
/// `proc_name` returns a slightly longer label on some macOS versions.
const PROC_NAME_MAXSIZE: usize = 256;

/// Prefix the VZ worker's `proc_name` / `p_comm` reliably starts with.
/// The full binary name is `com.apple.Virtualization.VirtualMachine`;
/// macOS truncates `p_comm` to a small buffer so we match the prefix.
const VZ_WORKER_NAME_PREFIX: &str = "com.apple.Virtual";

/// Full file name of the VZ worker executable. Used as a belt-and-braces
/// identity check via `proc_pidpath` if the `proc_name` prefix check fails.
const VZ_WORKER_FILE_NAME: &str = "com.apple.Virtualization.VirtualMachine";

/// All PIDs visible to the current process, via `proc_listallpids`.
///
/// Virtualization.framework does **not** `fork()` the VZ worker from the host
/// process — it launches it through launchd/XPC, so the worker's `ppid` is 1,
/// not our PID. We therefore have to scan the whole pid table and filter to
/// VZ workers by executable identity. The scan is cheap (a single
/// `proc_listallpids` syscall) and only happens on VM start / restore, not on
/// every `resource_usage()` read.
///
/// Returns an empty vector on FFI error.
pub(crate) fn all_pids() -> Vec<pid_t> {
    // First pass: ask the kernel how many bytes it would write.
    let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Vec::new();
    }

    // Over-allocate slightly: the pid table can grow between calls.
    let slack = (needed as usize) + 64 * std::mem::size_of::<pid_t>();
    let mut buf: Vec<pid_t> = vec![0; slack / std::mem::size_of::<pid_t>()];
    let written = unsafe {
        libc::proc_listallpids(
            buf.as_mut_ptr() as *mut libc::c_void,
            (buf.len() * std::mem::size_of::<pid_t>()) as c_int,
        )
    };
    if written <= 0 {
        return Vec::new();
    }

    let count = (written as usize) / std::mem::size_of::<pid_t>();
    buf.truncate(count);
    buf.retain(|&p| p > 0);
    buf
}

/// Short process name (≈ `p_comm`), or `None` on FFI error.
pub(crate) fn proc_comm(pid: pid_t) -> Option<String> {
    let mut buf = [0u8; PROC_NAME_MAXSIZE];
    let n =
        unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let slice = &buf[..(n as usize).min(buf.len())];
    std::str::from_utf8(slice).ok().map(|s| s.to_string())
}

/// Full executable path of a process, or `None` on FFI error.
pub(crate) fn proc_pidpath(pid: pid_t) -> Option<PathBuf> {
    let mut buf = [0u8; PROC_PIDPATHINFO_MAXSIZE];
    let n =
        unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let slice = &buf[..(n as usize).min(buf.len())];
    std::str::from_utf8(slice).ok().map(PathBuf::from)
}

/// Monotonic process-start time (mach absolute time units) from rusage V4.
/// Used as a strong PID-reuse guard.
pub(crate) fn proc_start_abstime(pid: pid_t) -> Option<u64> {
    proc_rusage_v4(pid).map(|info| info.ri_proc_start_abstime)
}

/// Sample `rusage_info_v4` for the given PID.
///
/// V4 was added in macOS 10.12 (Sierra, 2016) and is ABI-stable. It already
/// contains `ri_instructions`, `ri_cycles`, `ri_billed_energy`, and
/// `ri_interval_max_phys_footprint`, which is all we need for
/// [`crate::vm::metrics::ResourceUsage`]. Later `rusage_info_v5/v6` only
/// add fields (neural footprint, logical-reads) we do not report.
pub(crate) fn proc_rusage_v4(pid: pid_t) -> Option<libc::rusage_info_v4> {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // `proc_pid_rusage`'s third argument is declared `*mut rusage_info_t`
    // where `rusage_info_t = *mut c_void` — a type-pun for "pointer to a
    // rusage_info_vN struct". Cast through `*mut _` to satisfy it.
    let ret = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            &mut info as *mut libc::rusage_info_v4 as *mut libc::rusage_info_t,
        )
    };
    if ret == 0 {
        Some(info)
    } else {
        None
    }
}

/// True iff `pid` identifies a Virtualization.framework worker subprocess.
///
/// Matches either the truncated `proc_name` prefix (the common case) or
/// the full `proc_pidpath` file name (fallback for macOS versions that
/// present `p_comm` differently). Either match is sufficient.
pub(crate) fn is_vz_worker(pid: pid_t) -> bool {
    if let Some(name) = proc_comm(pid) {
        if name.starts_with(VZ_WORKER_NAME_PREFIX) {
            return true;
        }
    }
    if let Some(path) = proc_pidpath(pid) {
        if path
            .file_name()
            .map(|f| f == VZ_WORKER_FILE_NAME)
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// Current mach absolute time — the same clock as `rusage_info.ri_proc_start_abstime`.
///
/// Used to record "right before we called `startWithCompletionHandler`" so a
/// subsequent completion can identify workers forked *after* this moment.
///
/// `libc::mach_absolute_time` is `#[deprecated]` in favor of the `mach2`
/// crate, but we need the exact same clock that `ri_proc_start_abstime`
/// is expressed in, and we want zero new deps. Suppressed locally.
#[allow(deprecated)]
pub(crate) fn mach_absolute_time() -> u64 {
    // Safe to call from any thread.
    unsafe { libc::mach_absolute_time() }
}

/// Pick the VZ worker that this VM's `startWithCompletionHandler` just launched.
///
/// Each concurrent start in the same host process records a `submit_abstime`
/// just before calling the framework, then (on completion) asks this function
/// to pick the corresponding worker. The invariant:
///
/// > Our worker must have `proc_start_abstime > submit_abstime`, and must not
/// > already be claimed by another VM in this process.
///
/// Among all VZ workers satisfying both constraints, pick the one with the
/// *smallest* `proc_start_abstime` — that is the earliest launch after our
/// submission, which must be ours (later launches came from later submissions).
///
/// Because the VZ worker is XPC-launched (ppid = launchd), this scans all
/// system PIDs and filters to VZ workers. Cross-host-process disambiguation
/// is best-effort: if another process on the same Mac happens to start a VZ
/// VM in the exact same mach-abstime window, the first caller wins; the
/// other sees no candidate and returns `None` (and its `resource_usage()`
/// returns `None` until a subsequent sample / next start).
///
/// This needs no lock held across the framework's async completion; the
/// `already_claimed` set is consulted with a short lock at claim/release
/// time only.
pub(crate) fn pick_own_worker(
    submit_abstime: u64,
    already_claimed: &HashSet<pid_t>,
) -> Option<(pid_t, u64)> {
    let candidates: Vec<(pid_t, u64)> = all_pids()
        .into_iter()
        .filter(|&p| is_vz_worker(p))
        .filter_map(|p| proc_start_abstime(p).map(|t| (p, t)))
        .collect();
    pick_earliest_after(submit_abstime, &candidates, already_claimed)
}

/// Pure selection: among candidates, pick the one with the smallest
/// start-abstime greater than `submit_abstime`, excluding already-claimed PIDs.
/// Factored out for unit testability.
fn pick_earliest_after(
    submit_abstime: u64,
    candidates: &[(pid_t, u64)],
    already_claimed: &HashSet<pid_t>,
) -> Option<(pid_t, u64)> {
    candidates
        .iter()
        .copied()
        .filter(|&(p, t)| t > submit_abstime && !already_claimed.contains(&p))
        .min_by_key(|&(_, t)| t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn pick_earliest_after_returns_none_when_all_predate_submit() {
        let candidates = [(10, 1_000), (20, 2_000)];
        assert_eq!(
            pick_earliest_after(5_000, &candidates, &HashSet::new()),
            None
        );
    }

    #[test]
    fn pick_earliest_after_returns_none_for_empty_candidates() {
        assert_eq!(pick_earliest_after(0, &[], &HashSet::new()), None);
    }

    #[test]
    fn pick_earliest_after_picks_sole_candidate_newer_than_submit() {
        let candidates = [(42, 9_000)];
        assert_eq!(
            pick_earliest_after(1_000, &candidates, &HashSet::new()),
            Some((42, 9_000))
        );
    }

    #[test]
    fn pick_earliest_after_prefers_smallest_start_time_after_submit() {
        // Two workers forked after our submission; we want the earlier one
        // since later ones must belong to submissions that came after ours.
        let candidates = [(10, 2_000), (20, 5_000), (30, 3_000)];
        assert_eq!(
            pick_earliest_after(1_000, &candidates, &HashSet::new()),
            Some((10, 2_000))
        );
    }

    #[test]
    fn pick_earliest_after_skips_already_claimed_pids() {
        // The earliest candidate is claimed, so we fall through to the next.
        let candidates = [(10, 2_000), (20, 3_000), (30, 5_000)];
        let claimed: HashSet<pid_t> = [10].into_iter().collect();
        assert_eq!(
            pick_earliest_after(1_000, &candidates, &claimed),
            Some((20, 3_000))
        );
    }

    #[test]
    fn pick_earliest_after_filters_out_pids_at_or_before_submit() {
        // Strict `>` filter: a worker whose start_abstime equals submit_abstime
        // is NOT ours (too-fine-grained timing but correct by definition).
        let candidates = [(10, 5_000), (20, 5_001)];
        assert_eq!(
            pick_earliest_after(5_000, &candidates, &HashSet::new()),
            Some((20, 5_001))
        );
    }

    #[test]
    fn all_pids_includes_own_and_spawned_child() {
        let mut child = Command::new("/bin/sleep")
            .arg("3")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let child_pid = child.id() as pid_t;
        let own_pid = std::process::id() as pid_t;

        // Give the kernel a moment to register the child.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let pids = all_pids();
        let found_self = pids.contains(&own_pid);
        let found_child = pids.contains(&child_pid);

        // Clean up before asserting so we don't leak a sleep on failure
        // and don't leave a zombie behind.
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            found_self,
            "expected own pid {} in all_pids(), got len={}",
            own_pid,
            pids.len()
        );
        assert!(
            found_child,
            "expected child pid {} in all_pids(), got len={}",
            child_pid,
            pids.len()
        );
    }

    #[test]
    fn proc_comm_returns_something_for_own_pid() {
        let pid = std::process::id() as pid_t;
        let name = proc_comm(pid);
        assert!(
            name.as_deref().is_some_and(|s| !s.is_empty()),
            "proc_comm(self) returned {:?}",
            name
        );
    }

    #[test]
    fn proc_pidpath_returns_something_for_own_pid() {
        let pid = std::process::id() as pid_t;
        let path = proc_pidpath(pid);
        assert!(
            path.as_ref().map(|p| p.is_absolute()).unwrap_or(false),
            "proc_pidpath(self) returned {:?}",
            path
        );
    }

    #[test]
    fn proc_rusage_reports_cpu_time_for_own_pid() {
        // Burn a tiny bit of CPU so ri_user_time is non-zero.
        let mut acc: u64 = 0;
        for i in 0..200_000u64 {
            acc = acc.wrapping_add(i.wrapping_mul(31));
        }
        std::hint::black_box(acc);

        let pid = std::process::id() as pid_t;
        let info = proc_rusage_v4(pid).expect("rusage for self");
        assert!(
            info.ri_user_time + info.ri_system_time > 0,
            "expected non-zero cpu time"
        );
        assert!(info.ri_proc_start_abstime > 0, "expected start time set");
    }

    #[test]
    fn is_vz_worker_is_false_for_own_pid() {
        // This test runs in `cargo test`, which is not a VZ worker.
        let pid = std::process::id() as pid_t;
        assert!(!is_vz_worker(pid));
    }
}
