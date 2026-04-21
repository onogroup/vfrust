//! Thin FFI over Apple's `vmnet.framework`.
//!
//! Exposes just enough of the API to start a virtual network interface in
//! Shared / Host / Bridged mode, register a packets-available callback,
//! and pump packets through `vmnet_read` / `vmnet_write`. Higher-level
//! plumbing (socketpair bridge to VZ, byte counters, proxy threads) lives
//! in [`crate::vm::vmnet_proxy`].
//!
//! Reference: `<vmnet/vmnet.h>` (part of macOS SDK).
//!
//! All calls are gated behind the `com.apple.vm.networking` entitlement.
//! Non-profile-signed binaries additionally need root for Shared/Host
//! mode; Bridged mode in practice requires a provisioning profile.
//!
//! Several FFI symbols and helpers in this module are consumed by
//! `crate::vm::vmnet_proxy`, which is added in a follow-up commit.
//! Dead-code is silenced at module scope so this module is committable
//! in isolation.
#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::net::Ipv4Addr;
use std::os::raw::c_uint;
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex};

use block2::{Block, RcBlock};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};

use crate::config::device::network::{MacAddress, VmnetConfig, VmnetMode};

// ---------------------------------------------------------------------------
// Raw types
// ---------------------------------------------------------------------------

pub(crate) type InterfaceRef = *mut c_void;
pub(crate) type XpcObject = *mut c_void;

/// Return codes for `vmnet_*` functions. Values from `<vmnet/vmnet.h>`.
///
/// Re-exported for diagnostics — notably, [`VmnetProbe::Unavailable`]
/// carries one of these to describe why a vmnet start attempt failed.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
#[non_exhaustive]
pub enum VmnetReturn {
    Success = 1000,
    Failure = 1001,
    MemFailure = 1002,
    InvalidArgument = 1003,
    SetupIncomplete = 1004,
    InvalidAccess = 1005,
    PacketTooBig = 1006,
    BufferExhausted = 1007,
    TooManyPackets = 1008,
    SharingServiceBusy = 1009,
    Unknown = -1,
}

impl From<c_int> for VmnetReturn {
    fn from(v: c_int) -> Self {
        match v {
            1000 => Self::Success,
            1001 => Self::Failure,
            1002 => Self::MemFailure,
            1003 => Self::InvalidArgument,
            1004 => Self::SetupIncomplete,
            1005 => Self::InvalidAccess,
            1006 => Self::PacketTooBig,
            1007 => Self::BufferExhausted,
            1008 => Self::TooManyPackets,
            1009 => Self::SharingServiceBusy,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for VmnetReturn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Success => "success",
            Self::Failure => "generic failure",
            Self::MemFailure => "memory allocation failure",
            Self::InvalidArgument => "invalid argument",
            Self::SetupIncomplete => "setup incomplete",
            Self::InvalidAccess => "invalid access (entitlement missing or not root?)",
            Self::PacketTooBig => "packet too big",
            Self::BufferExhausted => "buffer exhausted",
            Self::TooManyPackets => "too many packets",
            Self::SharingServiceBusy => "sharing service busy (another Shared-mode iface active?)",
            Self::Unknown => "unknown return code",
        };
        f.write_str(s)
    }
}

/// Operating modes for `vmnet_start_interface`. Values from
/// `<vmnet/vmnet.h>`.
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum OperatingMode {
    Shared = 1,
    Host = 2,
    Bridged = 3,
}

impl From<VmnetMode> for OperatingMode {
    fn from(m: VmnetMode) -> Self {
        match m {
            VmnetMode::Shared => Self::Shared,
            VmnetMode::Host => Self::Host,
            VmnetMode::Bridged => Self::Bridged,
        }
    }
}

/// `VMNET_INTERFACE_PACKETS_AVAILABLE` — the only event we subscribe to.
const VMNET_INTERFACE_PACKETS_AVAILABLE: u32 = 1 << 0;

/// `struct vmpktdesc` from `<vmnet/vmnet.h>`.
#[repr(C)]
pub(crate) struct Vmpktdesc {
    pub vm_pkt_size: usize,
    pub vm_pkt_iov: *mut libc::iovec,
    pub vm_pkt_iovcnt: u32,
    pub vm_flags: u32,
}

// ---------------------------------------------------------------------------
// Extern declarations
// ---------------------------------------------------------------------------

#[link(name = "vmnet", kind = "framework")]
extern "C" {
    // Key names — all `extern const char * const` in vmnet.h.
    pub(crate) static vmnet_operation_mode_key: *const c_char;
    pub(crate) static vmnet_shared_interface_name_key: *const c_char;
    pub(crate) static vmnet_start_address_key: *const c_char;
    pub(crate) static vmnet_end_address_key: *const c_char;
    pub(crate) static vmnet_subnet_mask_key: *const c_char;
    pub(crate) static vmnet_enable_isolation_key: *const c_char;
    pub(crate) static vmnet_allocate_mac_address_key: *const c_char;
    pub(crate) static vmnet_mac_address_key: *const c_char;
    pub(crate) static vmnet_mtu_key: *const c_char;
    pub(crate) static vmnet_max_packet_size_key: *const c_char;
    pub(crate) static vmnet_interface_id_key: *const c_char;

    pub(crate) fn vmnet_start_interface(
        desc: XpcObject,
        queue: *const c_void,
        handler: &Block<dyn Fn(c_int, XpcObject)>,
    ) -> InterfaceRef;

    pub(crate) fn vmnet_stop_interface(
        iface: InterfaceRef,
        queue: *const c_void,
        handler: &Block<dyn Fn(c_int)>,
    ) -> c_int;

    pub(crate) fn vmnet_read(
        iface: InterfaceRef,
        pkts: *mut Vmpktdesc,
        pktcnt: *mut c_int,
    ) -> c_int;

    pub(crate) fn vmnet_write(
        iface: InterfaceRef,
        pkts: *mut Vmpktdesc,
        pktcnt: *mut c_int,
    ) -> c_int;

    pub(crate) fn vmnet_interface_set_event_callback(
        iface: InterfaceRef,
        event_mask: u32,
        queue: *const c_void,
        handler: *const Block<dyn Fn(u32, XpcObject)>,
    ) -> c_int;
}

// XPC helpers from libSystem.
#[link(name = "System", kind = "dylib")]
extern "C" {
    fn xpc_dictionary_create(
        keys: *const *const c_char,
        values: *const XpcObject,
        count: usize,
    ) -> XpcObject;
    fn xpc_dictionary_set_string(d: XpcObject, key: *const c_char, value: *const c_char);
    fn xpc_dictionary_set_uint64(d: XpcObject, key: *const c_char, value: u64);
    fn xpc_dictionary_set_bool(d: XpcObject, key: *const c_char, value: bool);
    fn xpc_dictionary_get_string(d: XpcObject, key: *const c_char) -> *const c_char;
    fn xpc_dictionary_get_uint64(d: XpcObject, key: *const c_char) -> u64;
    fn xpc_dictionary_get_bool(d: XpcObject, key: *const c_char) -> bool;
    fn xpc_release(obj: XpcObject);
}

// ---------------------------------------------------------------------------
// High-level error + safe-ish Rust wrappers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct VmnetError {
    pub code: VmnetReturn,
    pub context: &'static str,
    /// Optional free-form detail from a wrapped error (e.g. a
    /// malformed-config message from `build_start_dict`). `None` for
    /// errors that originate directly from a `vmnet_*` return code.
    pub detail: Option<String>,
}

impl std::fmt::Display for VmnetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Some(d) => write!(f, "{}: {} ({d})", self.context, self.code),
            None => write!(f, "{}: {}", self.context, self.code),
        }
    }
}

impl std::error::Error for VmnetError {}

impl From<VmnetError> for crate::Error {
    fn from(e: VmnetError) -> Self {
        crate::Error::InvalidDevice(format!("vmnet: {e}"))
    }
}

/// Metadata returned by vmnet after a successful `vmnet_start_interface`.
#[derive(Debug, Clone)]
pub(crate) struct VmnetStartParams {
    pub mac: MacAddress,
    pub mtu: u32,
    pub max_packet_size: u32,
    pub dhcp_start: Option<Ipv4Addr>,
    pub dhcp_end: Option<Ipv4Addr>,
    pub subnet_mask: Option<Ipv4Addr>,
}

/// Owned vmnet event-callback block, retained for the lifetime of a
/// running interface so vmnet can continue invoking it.
type EventBlock = RcBlock<dyn Fn(u32, XpcObject)>;

/// Owned handle to a running vmnet interface. Drop closes the interface
/// (best-effort — the caller is strongly encouraged to call
/// [`stop_interface`] explicitly so they can observe any stop error).
pub(crate) struct VmnetInterfaceHandle {
    iface: InterfaceRef,
    queue: DispatchRetained<DispatchQueue>,
    /// Held so the event callback block isn't dropped while vmnet may
    /// still invoke it. Cleared on stop.
    event_block: Mutex<Option<EventBlock>>,
}

// `InterfaceRef` is a raw pointer into vmnet's internal state. vmnet is
// documented thread-safe for packet I/O; the handle is moved between
// threads (one pump per direction) and is only dropped from the owner.
unsafe impl Send for VmnetInterfaceHandle {}
unsafe impl Sync for VmnetInterfaceHandle {}

impl VmnetInterfaceHandle {
    pub(crate) fn iface(&self) -> InterfaceRef {
        self.iface
    }
}

// ---------------------------------------------------------------------------
// XPC dict builder
// ---------------------------------------------------------------------------

/// Build the `xpc_object_t` start-description dictionary from a
/// [`VmnetConfig`]. Caller takes ownership; release with `xpc_release`.
///
/// Returns an error only for clearly invalid config combinations (e.g.
/// Bridged mode without an interface name) — everything else is
/// forwarded as-is to vmnet, which will reject what it doesn't like.
pub(crate) fn build_start_dict(cfg: &VmnetConfig) -> crate::Result<XpcObject> {
    if matches!(cfg.mode, VmnetMode::Bridged) && cfg.bridged_interface.is_none() {
        return Err(crate::Error::InvalidDevice(
            "VmnetConfig::bridged_interface is required when mode is Bridged".into(),
        ));
    }

    unsafe {
        let dict = xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0);
        if dict.is_null() {
            return Err(crate::Error::InvalidDevice(
                "xpc_dictionary_create returned null".into(),
            ));
        }

        let mode: OperatingMode = cfg.mode.into();
        xpc_dictionary_set_uint64(dict, vmnet_operation_mode_key, mode as u64);

        if let Some(ref iface) = cfg.bridged_interface {
            let c = CString::new(iface.as_str()).map_err(|_| {
                crate::Error::InvalidDevice(
                    "VmnetConfig::bridged_interface contains a NUL byte".into(),
                )
            })?;
            xpc_dictionary_set_string(dict, vmnet_shared_interface_name_key, c.as_ptr());
        }

        if let Some(addr) = cfg.start_address {
            let s = CString::new(addr.to_string()).unwrap();
            xpc_dictionary_set_string(dict, vmnet_start_address_key, s.as_ptr());
        }
        if let Some(addr) = cfg.end_address {
            let s = CString::new(addr.to_string()).unwrap();
            xpc_dictionary_set_string(dict, vmnet_end_address_key, s.as_ptr());
        }
        if let Some(mask) = cfg.subnet_mask {
            let s = CString::new(mask.to_string()).unwrap();
            xpc_dictionary_set_string(dict, vmnet_subnet_mask_key, s.as_ptr());
        }

        xpc_dictionary_set_bool(dict, vmnet_allocate_mac_address_key, cfg.allocate_mac);
        xpc_dictionary_set_bool(dict, vmnet_enable_isolation_key, cfg.isolated);

        Ok(dict)
    }
}

fn read_string(dict: XpcObject, key: *const c_char) -> Option<String> {
    unsafe {
        let ptr = xpc_dictionary_get_string(dict, key);
        if ptr.is_null() {
            None
        } else {
            Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
        }
    }
}

fn parse_params(dict: XpcObject) -> Result<VmnetStartParams, VmnetError> {
    unsafe {
        let mac_str = read_string(dict, vmnet_mac_address_key).ok_or(VmnetError {
            code: VmnetReturn::Failure,
            context: "vmnet returned params without a MAC address",
            detail: None,
        })?;
        let mac = MacAddress::parse(&mac_str).ok_or(VmnetError {
            code: VmnetReturn::Failure,
            context: "vmnet returned an unparseable MAC address",
            detail: None,
        })?;

        let mtu = xpc_dictionary_get_uint64(dict, vmnet_mtu_key) as u32;
        let max_packet_size = xpc_dictionary_get_uint64(dict, vmnet_max_packet_size_key) as u32;

        let dhcp_start = read_string(dict, vmnet_start_address_key)
            .and_then(|s| Ipv4Addr::from_str(&s).ok());
        let dhcp_end =
            read_string(dict, vmnet_end_address_key).and_then(|s| Ipv4Addr::from_str(&s).ok());
        let subnet_mask =
            read_string(dict, vmnet_subnet_mask_key).and_then(|s| Ipv4Addr::from_str(&s).ok());

        Ok(VmnetStartParams {
            mac,
            mtu,
            max_packet_size,
            dhcp_start,
            dhcp_end,
            subnet_mask,
        })
    }
}

// ---------------------------------------------------------------------------
// Start / stop (sync wrappers around async completion blocks)
// ---------------------------------------------------------------------------

/// Start a vmnet interface and block until the completion handler fires.
/// On success returns a live handle and the params dict (assigned MAC,
/// MTU, DHCP range).
///
/// Returns `VmnetError` (not `crate::Error`) so callers — in particular
/// [`crate::vm::vmnet_probe::probe_vmnet`] — can distinguish `InvalidAccess`
/// (hard entitlement denial) from `SharingServiceBusy` (transient) and
/// other return codes without string-matching.
pub(crate) fn start_interface(
    cfg: &VmnetConfig,
) -> Result<(VmnetInterfaceHandle, VmnetStartParams), VmnetError> {
    let desc = match build_start_dict(cfg) {
        Ok(d) => d,
        Err(e) => {
            // build_start_dict returns crate::Error::InvalidDevice for
            // malformed configs. Map to a synthetic VmnetError so the
            // callsite keeps a uniform error type.
            return Err(VmnetError {
                code: VmnetReturn::InvalidArgument,
                context: "build_start_dict",
                detail: Some(e.to_string()),
            });
        }
    };

    let queue = DispatchQueue::new("com.vfrust.vmnet", DispatchQueueAttr::SERIAL);

    // `(Condvar-guarded Option)` — filled by the completion block, drained
    // by this thread. Using Mutex+Condvar (not mpsc) so the RcBlock can be
    // Fn rather than FnOnce.
    type Slot = Mutex<Option<Result<VmnetStartParams, VmnetError>>>;
    let slot: Arc<(Slot, Condvar)> = Arc::new((Mutex::new(None), Condvar::new()));

    let slot_for_block = slot.clone();
    let handler = RcBlock::new(move |ret: c_int, params: XpcObject| {
        let code = VmnetReturn::from(ret);
        let outcome = if matches!(code, VmnetReturn::Success) {
            parse_params(params)
        } else {
            Err(VmnetError {
                code,
                context: "vmnet_start_interface completion",
                detail: None,
            })
        };
        let (lock, cvar) = &*slot_for_block;
        let mut guard = lock.lock().unwrap();
        *guard = Some(outcome);
        cvar.notify_all();
    });

    let iface = unsafe {
        let q_ptr: *const c_void = &*queue as *const DispatchQueue as *const c_void;
        vmnet_start_interface(desc, q_ptr, &handler)
    };
    unsafe { xpc_release(desc) };

    if iface.is_null() {
        // Synchronous rejection from vmnet_start_interface — the completion
        // handler will not fire. In practice this is the AMFI/entitlement
        // denial path on macOS 26+ ad-hoc signed binaries. Report it as
        // InvalidAccess so `probe_vmnet` can classify it as Denied.
        return Err(VmnetError {
            code: VmnetReturn::InvalidAccess,
            context: "vmnet_start_interface returned null iface",
            detail: Some("entitlement denied, binary not codesigned for vmnet, or insufficient privileges".into()),
        });
    }

    // Wait for completion.
    let (lock, cvar) = &*slot;
    let mut guard = lock.lock().unwrap();
    while guard.is_none() {
        guard = cvar.wait(guard).unwrap();
    }
    let params = guard.take().unwrap()?;

    Ok((
        VmnetInterfaceHandle {
            iface,
            queue,
            event_block: Mutex::new(None),
        },
        params,
    ))
}

/// Stop the interface and block until the completion handler fires.
/// Consumes the handle; the underlying `iface` is no longer valid after.
pub(crate) fn stop_interface(handle: VmnetInterfaceHandle) -> crate::Result<()> {
    // Drop the event callback block so vmnet doesn't re-invoke it during
    // shutdown.
    *handle.event_block.lock().unwrap() = None;

    let done: Arc<(Mutex<Option<c_int>>, Condvar)> =
        Arc::new((Mutex::new(None), Condvar::new()));
    let done_for_block = done.clone();
    let handler = RcBlock::new(move |ret: c_int| {
        let (lock, cvar) = &*done_for_block;
        *lock.lock().unwrap() = Some(ret);
        cvar.notify_all();
    });

    let submit = unsafe {
        let q_ptr: *const c_void = &*handle.queue as *const DispatchQueue as *const c_void;
        vmnet_stop_interface(handle.iface, q_ptr, &handler)
    };
    let submit_code = VmnetReturn::from(submit);
    if !matches!(submit_code, VmnetReturn::Success) {
        return Err(VmnetError {
            code: submit_code,
            context: "vmnet_stop_interface submit",
            detail: None,
        }
        .into());
    }

    let (lock, cvar) = &*done;
    let mut guard = lock.lock().unwrap();
    while guard.is_none() {
        guard = cvar.wait(guard).unwrap();
    }
    let code = VmnetReturn::from(guard.take().unwrap());
    if !matches!(code, VmnetReturn::Success) {
        return Err(VmnetError {
            code,
            context: "vmnet_stop_interface completion",
            detail: None,
        }
        .into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Packet I/O + event callback
// ---------------------------------------------------------------------------

/// Batched `vmnet_read`. `pkts` is populated in-place; returns the count
/// actually read.
pub(crate) fn read_packets(
    handle: &VmnetInterfaceHandle,
    pkts: &mut [Vmpktdesc],
) -> Result<usize, VmnetError> {
    let mut count = pkts.len() as c_int;
    let ret = unsafe { vmnet_read(handle.iface, pkts.as_mut_ptr(), &mut count) };
    let code = VmnetReturn::from(ret);
    if !matches!(code, VmnetReturn::Success) {
        return Err(VmnetError {
            code,
            context: "vmnet_read",
            detail: None,
        });
    }
    Ok(count.max(0) as usize)
}

/// `vmnet_write` a batch of packets. Returns the count actually written.
pub(crate) fn write_packets(
    handle: &VmnetInterfaceHandle,
    pkts: &mut [Vmpktdesc],
) -> Result<usize, VmnetError> {
    let mut count = pkts.len() as c_int;
    let ret = unsafe { vmnet_write(handle.iface, pkts.as_mut_ptr(), &mut count) };
    let code = VmnetReturn::from(ret);
    if !matches!(code, VmnetReturn::Success) {
        return Err(VmnetError {
            code,
            context: "vmnet_write",
            detail: None,
        });
    }
    Ok(count.max(0) as usize)
}

/// Register a callback that fires on `VMNET_INTERFACE_PACKETS_AVAILABLE`.
/// The callback is invoked on vmnet's own dispatch queue; callers should
/// do minimal work there (typically: notify a condvar that a pump thread
/// waits on).
pub(crate) fn set_packets_available_callback(
    handle: &VmnetInterfaceHandle,
    cb: impl Fn() + Send + Sync + 'static,
) -> Result<(), VmnetError> {
    let block: RcBlock<dyn Fn(u32, XpcObject)> =
        RcBlock::new(move |_events: u32, _params: XpcObject| {
            cb();
        });

    let ret = unsafe {
        let q_ptr: *const c_void = &*handle.queue as *const DispatchQueue as *const c_void;
        vmnet_interface_set_event_callback(
            handle.iface,
            VMNET_INTERFACE_PACKETS_AVAILABLE,
            q_ptr,
            &*block,
        )
    };
    let code = VmnetReturn::from(ret);
    if !matches!(code, VmnetReturn::Success) {
        return Err(VmnetError {
            code,
            context: "vmnet_interface_set_event_callback",
            detail: None,
        });
    }

    *handle.event_block.lock().unwrap() = Some(block);
    Ok(())
}

// Silence unused-import lint on c_uint in release builds that inline iovec helpers.
#[allow(dead_code)]
type _UnusedCUint = c_uint;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmnet_return_round_trip() {
        assert_eq!(VmnetReturn::from(1000), VmnetReturn::Success);
        assert_eq!(VmnetReturn::from(1005), VmnetReturn::InvalidAccess);
        assert_eq!(VmnetReturn::from(1009), VmnetReturn::SharingServiceBusy);
        assert_eq!(VmnetReturn::from(9999), VmnetReturn::Unknown);
    }

    #[test]
    fn vmnet_return_display_mentions_entitlement_for_invalid_access() {
        let s = format!("{}", VmnetReturn::InvalidAccess);
        assert!(s.contains("entitlement"), "got: {s}");
    }

    #[test]
    fn operating_mode_matches_vmnet_h_numbers() {
        assert_eq!(OperatingMode::Shared as u64, 1);
        assert_eq!(OperatingMode::Host as u64, 2);
        assert_eq!(OperatingMode::Bridged as u64, 3);
    }

    #[test]
    fn build_start_dict_rejects_bridged_without_interface() {
        let cfg = VmnetConfig {
            mode: VmnetMode::Bridged,
            bridged_interface: None,
            ..VmnetConfig::default()
        };
        assert!(build_start_dict(&cfg).is_err());
    }

    #[test]
    fn build_start_dict_accepts_shared_default() {
        // Smoke test: confirms the XPC dict helpers link and don't crash
        // on a default config. We don't introspect the dict (that would
        // require xpc_dictionary_get_count, which we haven't bound), but
        // null-check + release is a useful smoke signal.
        let cfg = VmnetConfig::default();
        let dict = build_start_dict(&cfg).expect("default should build");
        assert!(!dict.is_null());
        unsafe { xpc_release(dict) };
    }

    #[test]
    fn build_start_dict_accepts_host_with_custom_pool() {
        let cfg = VmnetConfig {
            mode: VmnetMode::Host,
            start_address: Some(Ipv4Addr::new(10, 0, 0, 2)),
            end_address: Some(Ipv4Addr::new(10, 0, 0, 100)),
            subnet_mask: Some(Ipv4Addr::new(255, 255, 255, 0)),
            allocate_mac: true,
            isolated: true,
            bridged_interface: None,
        };
        let dict = build_start_dict(&cfg).expect("host+pool should build");
        assert!(!dict.is_null());
        unsafe { xpc_release(dict) };
    }

    #[test]
    fn vmnet_error_message_includes_context() {
        let e = VmnetError {
            code: VmnetReturn::InvalidAccess,
            context: "testing",
            detail: None,
        };
        let s = format!("{e}");
        assert!(s.contains("testing"));
        assert!(s.contains("entitlement"));
    }
}
