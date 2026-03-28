//! Time synchronization: sync guest clock after host wakes from sleep.
//!
//! Uses IOKit power management notifications to detect host sleep/wake events,
//! then sends the current time to the guest via the QEMU Guest Agent (QGA)
//! `guest-set-time` command over vsock.

use std::ffi::c_void;
use std::time::{SystemTime, UNIX_EPOCH};

use core_foundation_sys::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRun,
};

/// Start the timesync background task.
///
/// Spawns a dedicated thread that listens for macOS sleep/wake events via IOKit.
/// When the host wakes from sleep, it connects to the guest over vsock and sets
/// the guest clock to the current host time using the QGA protocol.
pub fn start_timesync(handle: vfrust::VmHandle, vsock_port: u32, rt_handle: tokio::runtime::Handle) {
    std::thread::Builder::new()
        .name("timesync-wake-watcher".into())
        .spawn(move || {
            run_wake_watcher(handle, vsock_port, rt_handle);
        })
        .expect("failed to spawn timesync thread");
}

// ---------------------------------------------------------------------------
// QGA protocol
// ---------------------------------------------------------------------------

/// Send `guest-set-time` to the guest via QGA over vsock.
///
/// Connects to the guest on the given vsock port, sends the QGA JSON command
/// with the current time in nanoseconds since the Unix epoch, and reads the
/// response.
fn sync_guest_time(
    handle: &vfrust::VmHandle,
    vsock_port: u32,
    rt_handle: &tokio::runtime::Handle,
) {
    let now_ns = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(e) => {
            tracing::error!("timesync: failed to get current time: {e}");
            return;
        }
    };

    let command = format!(
        "{{\"execute\": \"guest-set-time\", \"arguments\": {{\"time\": {now_ns}}}}}\n"
    );

    tracing::info!("timesync: syncing guest time ({}ns since epoch)", now_ns);

    // connect_vsock is async, so we block on it from this OS thread.
    let conn = match rt_handle.block_on(vfrust::vsock::connect_vsock(handle, vsock_port)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("timesync: failed to connect to guest vsock port {vsock_port}: {e}");
            return;
        }
    };

    // Send the QGA command
    if let Err(e) = conn.write(command.as_bytes()) {
        tracing::warn!("timesync: failed to send QGA command: {e}");
        return;
    }

    // Read the response (best-effort; QGA returns {"return": {}}\n)
    let mut buf = [0u8; 1024];
    match conn.read(&mut buf) {
        Ok(n) => {
            let response = String::from_utf8_lossy(&buf[..n]);
            tracing::debug!("timesync: QGA response: {}", response.trim());
        }
        Err(e) => {
            tracing::warn!("timesync: failed to read QGA response: {e}");
        }
    }

    tracing::info!("timesync: guest time synchronized");
}

// ---------------------------------------------------------------------------
// IOKit FFI for power management notifications
// ---------------------------------------------------------------------------

#[allow(non_camel_case_types)]
mod iokit_ffi {
    use std::ffi::c_void;

    // IOKit types
    pub type IONotificationPortRef = *mut c_void;
    pub type io_object_t = u32;
    pub type io_connect_t = u32;
    pub type kern_return_t = i32;

    // Power management message types
    pub const K_IO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xe0000280;
    pub const K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xe0000300;
    pub const K_IO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xe0000270;

    /// Callback type for IORegisterForSystemPower.
    pub type IOServiceInterestCallback = unsafe extern "C" fn(
        refcon: *mut c_void,
        service: io_object_t,
        message_type: u32,
        message_argument: *mut c_void,
    );

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        pub fn IORegisterForSystemPower(
            refcon: *mut c_void,
            thePortRef: *mut IONotificationPortRef,
            callback: IOServiceInterestCallback,
            notifier: *mut io_object_t,
        ) -> io_connect_t;

        pub fn IONotificationPortGetRunLoopSource(
            notify: IONotificationPortRef,
        ) -> core_foundation_sys::runloop::CFRunLoopSourceRef;

        pub fn IODeregisterForSystemPower(notifier: *mut io_object_t) -> kern_return_t;

        pub fn IOAllowPowerChange(
            kernelPort: io_connect_t,
            notificationID: libc::c_long,
        ) -> kern_return_t;
    }
}

// ---------------------------------------------------------------------------
// Context passed through the IOKit callback refcon pointer
// ---------------------------------------------------------------------------

struct WakeContext {
    handle: vfrust::VmHandle,
    vsock_port: u32,
    rt_handle: tokio::runtime::Handle,
    root_port: iokit_ffi::io_connect_t,
}

// ---------------------------------------------------------------------------
// IOKit power callback
// ---------------------------------------------------------------------------

/// Called by IOKit on power management events (sleep, wake, etc.).
///
/// # Safety
///
/// `refcon` must point to a valid `WakeContext` that outlives the callback
/// registration. This is guaranteed because the context is heap-allocated
/// and leaked for the lifetime of the CFRunLoop thread.
unsafe extern "C" fn power_callback(
    refcon: *mut c_void,
    _service: iokit_ffi::io_object_t,
    message_type: u32,
    message_argument: *mut c_void,
) {
    let ctx = unsafe { &*(refcon as *const WakeContext) };

    match message_type {
        iokit_ffi::K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON => {
            tracing::info!("timesync: host woke from sleep, syncing guest time");
            sync_guest_time(&ctx.handle, ctx.vsock_port, &ctx.rt_handle);
        }
        iokit_ffi::K_IO_MESSAGE_SYSTEM_WILL_SLEEP => {
            // We must acknowledge the sleep notification to allow the system
            // to proceed with sleeping.
            tracing::debug!("timesync: system going to sleep, acknowledging");
            unsafe {
                iokit_ffi::IOAllowPowerChange(ctx.root_port, message_argument as libc::c_long);
            }
        }
        iokit_ffi::K_IO_MESSAGE_CAN_SYSTEM_SLEEP => {
            // Idle sleep notification -- we allow it.
            tracing::debug!("timesync: system idle sleep, acknowledging");
            unsafe {
                iokit_ffi::IOAllowPowerChange(ctx.root_port, message_argument as libc::c_long);
            }
        }
        _ => {
            tracing::trace!("timesync: ignoring power message 0x{:08x}", message_type);
        }
    }
}

// ---------------------------------------------------------------------------
// Wake watcher (runs on dedicated thread)
// ---------------------------------------------------------------------------

/// Register for IOKit power notifications and run a CFRunLoop to receive them.
///
/// This function blocks the calling thread indefinitely (the CFRunLoop runs
/// until the process exits). It should be called from a dedicated thread.
fn run_wake_watcher(
    handle: vfrust::VmHandle,
    vsock_port: u32,
    rt_handle: tokio::runtime::Handle,
) {
    let mut notify_port: iokit_ffi::IONotificationPortRef = std::ptr::null_mut();
    let mut notifier: iokit_ffi::io_object_t = 0;

    // Allocate the context on the heap and leak it so it lives for the
    // lifetime of this thread. The pointer is passed as the refcon to IOKit.
    // We fill in root_port after the IORegisterForSystemPower call.
    let ctx = Box::new(WakeContext {
        handle,
        vsock_port,
        rt_handle,
        root_port: 0,
    });
    let ctx_ptr = Box::into_raw(ctx);

    let root_port = unsafe {
        iokit_ffi::IORegisterForSystemPower(
            ctx_ptr as *mut c_void,
            &mut notify_port,
            power_callback,
            &mut notifier,
        )
    };

    if root_port == 0 {
        tracing::error!("timesync: IORegisterForSystemPower failed");
        // Clean up the leaked context
        unsafe {
            drop(Box::from_raw(ctx_ptr));
        }
        return;
    }

    // Now fill in the root_port so the callback can acknowledge sleep.
    unsafe {
        (*ctx_ptr).root_port = root_port;
    }

    let rls = unsafe { iokit_ffi::IONotificationPortGetRunLoopSource(notify_port) };
    if rls.is_null() {
        tracing::error!("timesync: IONotificationPortGetRunLoopSource returned null");
        unsafe {
            iokit_ffi::IODeregisterForSystemPower(&mut notifier);
            drop(Box::from_raw(ctx_ptr));
        }
        return;
    }

    unsafe {
        CFRunLoopAddSource(CFRunLoopGetCurrent(), rls, kCFRunLoopDefaultMode);
    }

    tracing::info!("timesync: wake watcher started on vsock port {vsock_port}");

    // Also sync time once on startup, in case the VM was started while
    // the host was already running and the guest clock is stale.
    unsafe {
        let ctx = &*ctx_ptr;
        sync_guest_time(&ctx.handle, ctx.vsock_port, &ctx.rt_handle);
    }

    // Run the CFRunLoop -- this blocks forever (until the process exits).
    unsafe {
        CFRunLoopRun();
    }

    // Cleanup (reached only if CFRunLoop is explicitly stopped, which we
    // don't do, but for correctness):
    unsafe {
        iokit_ffi::IODeregisterForSystemPower(&mut notifier);
        drop(Box::from_raw(ctx_ptr));
    }
}
