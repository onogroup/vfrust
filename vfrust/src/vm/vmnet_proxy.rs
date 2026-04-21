//! A `vmnet.framework` ↔ `VZFileHandleNetworkDeviceAttachment` bridge with
//! per-NIC byte / packet counters.
//!
//! vmnet's packet API (`vmnet_read` / `vmnet_write`) does not plug into
//! Virtualization.framework directly. The standard pattern — used by UTM,
//! vfkit, Tart, krunkit — is to bridge it through a `SOCK_DGRAM`
//! `socketpair`: one end handed to VZ via
//! `VZFileHandleNetworkDeviceAttachment`, the other end pumped from/to
//! vmnet by userspace threads. Because every frame crosses our pumps we
//! get free byte / packet counters along the way.
//!
//! Layout:
//!
//! ```text
//!   guest NIC ─┐                                   ┌── host network stack
//!              │                                   │
//!              └─ vz_fd ─[ SOCK_DGRAM socketpair ]─┴─ host_fd ──(pump)── vmnet iface
//! ```
//!
//! Shutdown ordering is load-bearing: `vmnet_stop_interface` must fully
//! complete before the bridge interface (`bridge100`, etc.) is released
//! by the host. Drop blocks on that completion.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use crate::config::device::network::{MacAddress, VmnetConfig};
use crate::error::Error;
use crate::sys::vmnet::{
    read_packets, set_packets_available_callback, start_interface, stop_interface, Vmpktdesc,
    VmnetInterfaceHandle, VmnetStartParams,
};

/// Batch size for `vmnet_read` — matches the "small handful" other
/// vmnet consumers use; larger batches don't help because packets are
/// written to the socket one at a time.
const READ_BATCH: usize = 64;

/// Atomic per-NIC counters. All writes go through `fetch_add(…, Relaxed)`
/// from the pump threads; sampling reads are also `Relaxed`. Snapshots
/// are a best-effort mixture — acceptable because these are observability
/// counters, not control-plane state.
#[derive(Default)]
pub(crate) struct NetworkCounters {
    pub rx_bytes: AtomicU64,
    pub tx_bytes: AtomicU64,
    pub rx_packets: AtomicU64,
    pub tx_packets: AtomicU64,
    pub rx_drops: AtomicU64,
    pub tx_drops: AtomicU64,
}

/// Raw counter snapshot. The public `NetworkUsage` type (added in a
/// follow-up commit) wraps this along with `sampled_at`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NetworkCounterSnapshot {
    pub sampled_at: SystemTime,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_drops: u64,
    pub tx_drops: u64,
}

impl NetworkCounters {
    fn snapshot(&self) -> NetworkCounterSnapshot {
        NetworkCounterSnapshot {
            sampled_at: SystemTime::now(),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            rx_packets: self.rx_packets.load(Ordering::Relaxed),
            tx_packets: self.tx_packets.load(Ordering::Relaxed),
            rx_drops: self.rx_drops.load(Ordering::Relaxed),
            tx_drops: self.tx_drops.load(Ordering::Relaxed),
        }
    }
}

/// Fixed metadata about the running vmnet interface — the assigned MAC,
/// MTU, DHCP pool, etc.
#[derive(Debug, Clone)]
pub(crate) struct VmnetInterfaceInfo {
    pub mac: MacAddress,
    pub mtu: u32,
    pub max_packet_size: u32,
    pub dhcp_start: Option<std::net::Ipv4Addr>,
    pub dhcp_end: Option<std::net::Ipv4Addr>,
    pub subnet_mask: Option<std::net::Ipv4Addr>,
}

impl From<&VmnetStartParams> for VmnetInterfaceInfo {
    fn from(p: &VmnetStartParams) -> Self {
        Self {
            mac: p.mac.clone(),
            mtu: p.mtu,
            max_packet_size: p.max_packet_size,
            dhcp_start: p.dhcp_start,
            dhcp_end: p.dhcp_end,
            subnet_mask: p.subnet_mask,
        }
    }
}

/// Per-VM vmnet bridge. Created during VM build, torn down on stop / drop.
///
/// `VmnetProxy` is stored in an `Arc` on `InnerMachine` so:
///   - Lifecycle ends when `InnerMachine` drops (handle clears proxies).
///   - `VmHandle::network_usage` can clone the Arc to sample counters
///     without holding the inner lock across the sample call.
pub(crate) struct VmnetProxy {
    /// Wrapped in a `Mutex<Option<Arc<_>>>` so `stop` can drain the Arc
    /// after the pumps have released their clones, then call
    /// `stop_interface` on the uniquely-owned handle.
    iface: Mutex<Option<Arc<VmnetInterfaceHandle>>>,
    info: VmnetInterfaceInfo,
    counters: Arc<NetworkCounters>,
    host_fd: Arc<OwnedFd>,
    /// VZ's end of the socketpair, handed to
    /// `VZFileHandleNetworkDeviceAttachment::initWithFileHandle(..,
    /// closeOnDealloc=true)` so NSFileHandle closes it when VZ releases
    /// the attachment.  Stored as -1 once taken.
    vz_fd: Mutex<RawFd>,
    shutdown: Arc<AtomicBool>,
    wake: Arc<(Mutex<bool>, Condvar)>,
    pumps: Mutex<Vec<JoinHandle<()>>>,
    stopped: AtomicBool,
}

// All fields are `Send + Sync`. `VmnetInterfaceHandle` asserts this
// explicitly; `OwnedFd` is `Send + Sync`; the rest are standard.
unsafe impl Send for VmnetProxy {}
unsafe impl Sync for VmnetProxy {}

impl VmnetProxy {
    /// Start a vmnet interface and spin up the userspace pump threads.
    pub(crate) fn start(cfg: &VmnetConfig) -> crate::Result<Arc<Self>> {
        let (iface, params) = start_interface(cfg)?;
        let info: VmnetInterfaceInfo = (&params).into();

        let (host_raw, vz_raw) = make_socketpair(params.max_packet_size as usize)?;
        let host_fd = Arc::new(unsafe { OwnedFd::from_raw_fd(host_raw) });

        let counters = Arc::new(NetworkCounters::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let iface = Arc::new(iface);

        // Packet-available callback: just flip the flag and wake the pump.
        // vmnet invokes this on its own serial queue, so we keep it tiny.
        {
            let wake = wake.clone();
            set_packets_available_callback(&iface, move || {
                let (lock, cvar) = &*wake;
                if let Ok(mut ready) = lock.lock() {
                    *ready = true;
                    cvar.notify_all();
                }
            })
            .map_err(Error::from)?;
        }

        // vmnet → host_fd pump.
        let vmnet_to_fd = spawn_vmnet_to_fd_pump(
            iface.clone(),
            host_fd.clone(),
            counters.clone(),
            shutdown.clone(),
            wake.clone(),
            params.max_packet_size as usize,
        );

        // host_fd → vmnet pump.
        let fd_to_vmnet = spawn_fd_to_vmnet_pump(
            iface.clone(),
            host_fd.clone(),
            counters.clone(),
            shutdown.clone(),
            params.max_packet_size as usize,
        );

        Ok(Arc::new(VmnetProxy {
            iface: Mutex::new(Some(iface)),
            info,
            counters,
            host_fd,
            vz_fd: Mutex::new(vz_raw),
            shutdown,
            wake,
            pumps: Mutex::new(vec![vmnet_to_fd, fd_to_vmnet]),
            stopped: AtomicBool::new(false),
        }))
    }

    /// The raw fd to hand to `VZFileHandleNetworkDeviceAttachment`. Once
    /// taken, callers must pass `closeOnDealloc=true` to NSFileHandle so
    /// VZ closes it on teardown. Subsequent calls return `-1`.
    pub(crate) fn take_vz_fd(&self) -> RawFd {
        let mut slot = self.vz_fd.lock().unwrap();
        let fd = *slot;
        *slot = -1;
        fd
    }

    /// Per-NIC metadata — MAC, MTU, DHCP range.
    pub(crate) fn info(&self) -> VmnetInterfaceInfo {
        self.info.clone()
    }

    /// Sample the current byte / packet counters.
    pub(crate) fn sample(&self) -> NetworkCounterSnapshot {
        self.counters.snapshot()
    }

    /// Stop the pumps and tear down the vmnet interface. Idempotent; safe
    /// to call from either explicit shutdown or `Drop`.
    pub(crate) fn stop(&self) {
        if self
            .stopped
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the vmnet→fd pump.
        {
            let (lock, cvar) = &*self.wake;
            if let Ok(mut ready) = lock.lock() {
                *ready = true;
                cvar.notify_all();
            }
        }
        // Unblock the fd→vmnet pump's blocking read().
        unsafe {
            libc::shutdown(self.host_fd.as_raw_fd(), libc::SHUT_RDWR);
        }

        let pumps = std::mem::take(&mut *self.pumps.lock().unwrap());
        for jh in pumps {
            let _ = jh.join();
        }

        // Pumps have returned, so their `Arc<VmnetInterfaceHandle>`
        // clones are dropped. The proxy's own Arc is the only one left;
        // take it out of the mutex and unwrap it into an owned handle so
        // we can call the consuming `stop_interface`.
        let iface_arc = self.iface.lock().unwrap().take();
        if let Some(arc) = iface_arc {
            match Arc::try_unwrap(arc) {
                Ok(handle) => {
                    let _ = stop_interface(handle);
                }
                Err(arc) => {
                    // Some other clone still exists — surface this in logs
                    // and drop the Arc. vmnet will tear down when the last
                    // ref drops, at which point the interface leaks until
                    // the process exits. This should not happen in normal
                    // use and indicates a bug in lifecycle ordering.
                    tracing::warn!(
                        strong = Arc::strong_count(&arc),
                        "vmnet interface still has outstanding refs at stop; \
                         skipping explicit vmnet_stop_interface"
                    );
                }
            }
        }
    }
}

impl Drop for VmnetProxy {
    fn drop(&mut self) {
        // Drop-time stop is best-effort. Any hung pump during VM crash
        // would block here; the alternative (detaching threads) leaks the
        // bridge interface until reboot, which is worse.
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Pump threads
// ---------------------------------------------------------------------------

fn spawn_vmnet_to_fd_pump(
    iface: Arc<VmnetInterfaceHandle>,
    host_fd: Arc<OwnedFd>,
    counters: Arc<NetworkCounters>,
    shutdown: Arc<AtomicBool>,
    wake: Arc<(Mutex<bool>, Condvar)>,
    max_pkt: usize,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("vfrust-vmnet-rx".into())
        .spawn(move || {
            let mut bufs: Vec<Vec<u8>> = (0..READ_BATCH).map(|_| vec![0u8; max_pkt]).collect();

            loop {
                if shutdown.load(Ordering::SeqCst) {
                    return;
                }

                // Wait for vmnet to signal packets-available.
                {
                    let (lock, cvar) = &*wake;
                    let mut ready = lock.lock().unwrap();
                    while !*ready && !shutdown.load(Ordering::SeqCst) {
                        ready = cvar.wait(ready).unwrap();
                    }
                    *ready = false;
                }
                if shutdown.load(Ordering::SeqCst) {
                    return;
                }

                // Drain vmnet in batches until it reports 0.
                loop {
                    let (pkts_read, totals) = match drain_vmnet_once(&iface, &mut bufs) {
                        Ok(v) => v,
                        Err(_) => {
                            // Log in caller integration; spurious errors here
                            // would spam, so count a drop and continue.
                            counters.rx_drops.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                    };
                    if pkts_read == 0 {
                        break;
                    }

                    for (i, bytes) in totals.iter().enumerate().take(pkts_read) {
                        if shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        let n = *bytes;
                        let buf = &bufs[i][..n];
                        let written = unsafe {
                            libc::write(
                                host_fd.as_raw_fd(),
                                buf.as_ptr() as *const libc::c_void,
                                n,
                            )
                        };
                        if written < 0 {
                            let err = std::io::Error::last_os_error();
                            if matches!(err.kind(), std::io::ErrorKind::BrokenPipe)
                                || shutdown.load(Ordering::SeqCst)
                            {
                                return;
                            }
                            counters.rx_drops.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        counters.rx_packets.fetch_add(1, Ordering::Relaxed);
                        counters.rx_bytes.fetch_add(written as u64, Ordering::Relaxed);
                    }
                }
            }
        })
        .expect("spawn vmnet-rx pump")
}

fn spawn_fd_to_vmnet_pump(
    iface: Arc<VmnetInterfaceHandle>,
    host_fd: Arc<OwnedFd>,
    counters: Arc<NetworkCounters>,
    shutdown: Arc<AtomicBool>,
    max_pkt: usize,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("vfrust-vmnet-tx".into())
        .spawn(move || {
            let mut buf = vec![0u8; max_pkt];

            loop {
                if shutdown.load(Ordering::SeqCst) {
                    return;
                }

                // SOCK_DGRAM: each read is exactly one packet.
                let n = unsafe {
                    libc::read(
                        host_fd.as_raw_fd(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    )
                };
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if matches!(err.kind(), std::io::ErrorKind::Interrupted) {
                        continue;
                    }
                    return;
                }
                if n == 0 {
                    // Socket closed or shutdown'd.
                    return;
                }
                let n = n as usize;

                let mut iov = libc::iovec {
                    iov_base: buf.as_mut_ptr() as *mut libc::c_void,
                    iov_len: n,
                };
                let mut pkt = Vmpktdesc {
                    vm_pkt_size: n,
                    vm_pkt_iov: &mut iov as *mut libc::iovec,
                    vm_pkt_iovcnt: 1,
                    vm_flags: 0,
                };

                // `write_packets` takes `&mut [Vmpktdesc]` so we pass a
                // one-element slice.
                let slice = std::slice::from_mut(&mut pkt);
                match crate::sys::vmnet::write_packets(&iface, slice) {
                    Ok(1) => {
                        counters.tx_packets.fetch_add(1, Ordering::Relaxed);
                        counters.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Ok(_) => {
                        counters.tx_drops.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        counters.tx_drops.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
        .expect("spawn vmnet-tx pump")
}

/// A single call to `vmnet_read`. Returns (count read, per-slot byte lens).
fn drain_vmnet_once(
    iface: &VmnetInterfaceHandle,
    bufs: &mut [Vec<u8>],
) -> Result<(usize, Vec<usize>), crate::error::Error> {
    let max_pkt = bufs[0].capacity();

    // Build iovec + Vmpktdesc arrays pointing into `bufs`.
    // `iovec`/`Vmpktdesc` are `!Send + !Sync` but we only use them on
    // this thread, so stack-allocated arrays are fine.
    let mut iovs: Vec<libc::iovec> = bufs
        .iter_mut()
        .map(|b| libc::iovec {
            iov_base: b.as_mut_ptr() as *mut libc::c_void,
            iov_len: max_pkt,
        })
        .collect();

    let mut pkts: Vec<Vmpktdesc> = (0..bufs.len())
        .map(|i| Vmpktdesc {
            vm_pkt_size: max_pkt,
            vm_pkt_iov: &mut iovs[i] as *mut libc::iovec,
            vm_pkt_iovcnt: 1,
            vm_flags: 0,
        })
        .collect();

    let read = read_packets(iface, &mut pkts).map_err(Error::from)?;

    // `vm_pkt_size` is updated in-place to the actual payload length.
    let totals: Vec<usize> = pkts.iter().map(|p| p.vm_pkt_size).collect();
    Ok((read, totals))
}

// ---------------------------------------------------------------------------
// socketpair
// ---------------------------------------------------------------------------

/// SOCK_DGRAM pair sized to hold `max_pkt_size * READ_BATCH` worth of
/// buffered frames in each direction, so a brief stall in either pump
/// doesn't drop packets.
fn make_socketpair(max_pkt_size: usize) -> crate::Result<(RawFd, RawFd)> {
    let mut fds = [0i32; 2];
    let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) };
    if ret != 0 {
        return Err(Error::InvalidDevice(format!(
            "socketpair(AF_UNIX, SOCK_DGRAM) failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    let bufsz = (max_pkt_size.max(2048) * READ_BATCH).clamp(64 * 1024, 8 * 1024 * 1024);
    for fd in &fds {
        set_sockbuf(*fd, libc::SO_SNDBUF, bufsz);
        set_sockbuf(*fd, libc::SO_RCVBUF, bufsz);
    }
    Ok((fds[0], fds[1]))
}

fn set_sockbuf(fd: RawFd, option: libc::c_int, bytes: usize) {
    let val: libc::c_int = bytes.min(i32::MAX as usize) as libc::c_int;
    unsafe {
        let _ = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            &val as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

// `Read`/`Write` impls left unused but provided in case integration wants
// to test the fd pair directly.
#[allow(dead_code)]
struct _ReadMarker(MaybeUninit<u8>);
#[allow(dead_code)]
fn _ensure_traits_used(mut r: std::fs::File, buf: &mut [u8], data: &[u8]) {
    let _ = r.read(buf);
    let _ = r.write_all(data);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_default_zero_and_snapshot_is_self_consistent() {
        let c = NetworkCounters::default();
        let s = c.snapshot();
        assert_eq!(s.rx_bytes, 0);
        assert_eq!(s.tx_bytes, 0);
        assert_eq!(s.rx_packets, 0);
        assert_eq!(s.tx_packets, 0);
    }

    #[test]
    fn counters_fetch_add_is_visible_to_snapshot() {
        let c = NetworkCounters::default();
        c.rx_bytes.fetch_add(42, Ordering::Relaxed);
        c.tx_packets.fetch_add(7, Ordering::Relaxed);
        let s = c.snapshot();
        assert_eq!(s.rx_bytes, 42);
        assert_eq!(s.tx_packets, 7);
    }

    #[test]
    fn socketpair_is_bidirectional_dgram() {
        let (a, b) = make_socketpair(4096).expect("socketpair");
        let msg = b"hello";
        let wrote = unsafe {
            libc::write(
                a,
                msg.as_ptr() as *const libc::c_void,
                msg.len(),
            )
        };
        assert_eq!(wrote, msg.len() as isize);
        let mut buf = [0u8; 32];
        let read = unsafe {
            libc::read(b, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        };
        assert_eq!(read, msg.len() as isize);
        assert_eq!(&buf[..msg.len()], msg);
        unsafe {
            libc::close(a);
            libc::close(b);
        }
    }
}
