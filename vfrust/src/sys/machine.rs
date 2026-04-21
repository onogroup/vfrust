use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use libc::pid_t;
use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_foundation::NSError;
use objc2_virtualization::{VZVirtualMachine, VZVirtualMachineConfiguration, VZVirtualMachineState};
use tokio::sync::{mpsc, oneshot, watch};

use crate::config::vm::VmConfig;
use crate::sys::config_builder::build_vz_config;
use crate::sys::delegate::{DelegateEvent, VmDelegate};
use crate::sys::process_info;
use crate::vm::state::VmState;
use crate::vm::vmnet_proxy::VmnetProxy;

/// Shared list of `VmnetProxy` instances attached to a VM. Mirrors the
/// `WorkerSlot` pattern — lives on `InnerMachine`, cloned into `VmHandle`
/// for sampling.
pub(crate) type NetworkProxies = Arc<Mutex<Vec<Arc<VmnetProxy>>>>;

/// Identity of the `com.apple.Virtualization.VirtualMachine` worker subprocess
/// that backs a running `VZVirtualMachine`. Stashed at start-completion time
/// so per-sample rusage reads can verify the process is still ours
/// (PID-reuse guard via `start_abstime`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkerIdentity {
    pub pid: pid_t,
    pub start_abstime: u64,
}

pub(crate) type WorkerSlot = Arc<Mutex<Option<WorkerIdentity>>>;

/// Process-wide registry of VZ worker PIDs already attributed to a
/// running VM in this host process. Consulted with a short-lived lock
/// at claim/release time only — never held across async completion.
///
/// Concurrent starts are disambiguated by:
///   (a) recording `submit_abstime = mach_absolute_time()` on each
///       VM's serial queue *before* calling `startWithCompletionHandler`,
///   (b) on completion, among VZ workers with
///       `proc_start_abstime > submit_abstime` and not in this set,
///       picking the one with the *smallest* `proc_start_abstime`
///       (earliest fork after our submission must be ours — any later
///       worker came from a later submission).
fn claimed_workers() -> &'static Mutex<HashSet<pid_t>> {
    static SET: OnceLock<Mutex<HashSet<pid_t>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Wraps a raw pointer to VZVirtualMachine as usize for Send safety.
/// The pointer is only dereferenced inside closures dispatched to the VM's
/// own serial queue where the Retained<VZVirtualMachine> is kept alive.
#[derive(Clone, Copy)]
pub(crate) struct VmPtr(usize);
unsafe impl Send for VmPtr {}
unsafe impl Sync for VmPtr {}

impl VmPtr {
    fn new(vm: &VZVirtualMachine) -> Self {
        Self(vm as *const VZVirtualMachine as usize)
    }

    /// # Safety
    /// Must only be called on the VM's dispatch queue while the VM is alive.
    pub(crate) unsafe fn get(&self) -> &VZVirtualMachine {
        &*(self.0 as *const VZVirtualMachine)
    }
}

#[allow(dead_code)]
pub(crate) fn vz_state_to_rust(state: VZVirtualMachineState) -> VmState {
    match state {
        VZVirtualMachineState::Stopped => VmState::Stopped,
        VZVirtualMachineState::Running => VmState::Running,
        VZVirtualMachineState::Paused => VmState::Paused,
        VZVirtualMachineState::Error => VmState::Error,
        VZVirtualMachineState::Starting => VmState::Starting,
        VZVirtualMachineState::Pausing => VmState::Pausing,
        VZVirtualMachineState::Resuming => VmState::Resuming,
        VZVirtualMachineState::Stopping => VmState::Stopping,
        VZVirtualMachineState::Saving => VmState::Saving,
        VZVirtualMachineState::Restoring => VmState::Restoring,
        _ => VmState::Error,
    }
}

/// Record the freshly-forked VZ worker PID after a successful start/restore.
///
/// Picks the VZ-worker child whose `proc_start_abstime` is strictly greater
/// than `submit_abstime` (recorded just before `startWithCompletionHandler`)
/// and not already claimed by another concurrent start in this process. On
/// success stashes the PID plus its start-abstime (used later as a
/// PID-reuse guard) and marks the PID as claimed.
fn populate_worker_on_success(submit_abstime: u64, worker: &WorkerSlot) {
    // Snapshot `already_claimed` under a short-lived lock, then run the
    // selection outside the lock so rusage syscalls don't block other VMs.
    let claimed_snapshot: HashSet<pid_t> = claimed_workers()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // The VZ worker is XPC-launched asynchronously, so it may not appear in
    // `proc_listallpids` in the same instant that the start completion block
    // fires. A handful of short retries cheaply absorb that race window
    // without adding any observable latency on the happy path.
    let mut picked = None;
    for attempt in 0..10 {
        if let Some(hit) = process_info::pick_own_worker(submit_abstime, &claimed_snapshot) {
            picked = Some(hit);
            break;
        }
        if attempt < 9 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    let Some((pid, start_abstime)) = picked else {
        tracing::warn!(
            submit_abstime,
            "vz worker pid discovery: no com.apple.Virtualization.* worker visible after submit"
        );
        return;
    };

    // Re-acquire the registry lock and insert; if someone else claimed the
    // same PID between our snapshot and insertion (shouldn't happen, but be
    // defensive), drop this candidate rather than double-attribute.
    if let Ok(mut claimed) = claimed_workers().lock() {
        if !claimed.insert(pid) {
            tracing::warn!(pid, "vz worker pid already claimed by another VM; skipping");
            return;
        }
    }

    tracing::debug!(pid, start_abstime, submit_abstime, "vz worker discovered");
    if let Ok(mut slot) = worker.lock() {
        *slot = Some(WorkerIdentity { pid, start_abstime });
    }
}

/// Clear the tracked worker identity and release its claim on the
/// process-wide registry. Called on stop/error transitions.
fn clear_worker(worker: &WorkerSlot, reason: &str) {
    let released_pid = worker
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
        .map(|id| id.pid);
    if let Some(pid) = released_pid {
        if let Ok(mut claimed) = claimed_workers().lock() {
            claimed.remove(&pid);
        }
        tracing::debug!(pid, reason, "vz worker cleared");
    }
}

/// Helper to create a completion handler block from a oneshot sender.
/// Wraps the sender in Mutex<Option<>> so the block is Fn (not FnOnce).
/// Must be called on the dispatch queue thread (RcBlock is !Send).
pub(crate) fn make_completion_block(
    reply: Mutex<Option<oneshot::Sender<crate::Result<()>>>>,
    state_tx: Option<(watch::Sender<VmState>, VmState, VmState)>,
) -> RcBlock<dyn Fn(*mut NSError)> {
    RcBlock::new(move |err: *mut NSError| {
        if let Some(reply) = reply.lock().unwrap().take() {
            if err.is_null() {
                if let Some((ref tx, ok_state, _)) = state_tx {
                    let _ = tx.send(ok_state);
                }
                let _ = reply.send(Ok(()));
            } else {
                let error = unsafe { &*err };
                if let Some((ref tx, _, err_state)) = state_tx {
                    let _ = tx.send(err_state);
                }
                let _ = reply.send(Err(crate::sys::ns_error_to_error(error)));
            }
        }
    })
}

/// Completion-handler variant that additionally attributes a freshly-forked
/// VZ worker PID to this VM on success.
///
/// Identical to [`make_completion_block`] except that, on the success branch
/// (before forwarding state + reply), it calls
/// [`populate_worker_on_success`] so subsequent `resource_usage()` calls can
/// find the right process. Used by `dispatch_start` / `dispatch_restore_state`.
pub(crate) fn make_start_like_completion_block(
    reply: Mutex<Option<oneshot::Sender<crate::Result<()>>>>,
    state_tx: watch::Sender<VmState>,
    ok_state: VmState,
    err_state: VmState,
    submit_abstime: u64,
    worker: WorkerSlot,
) -> RcBlock<dyn Fn(*mut NSError)> {
    RcBlock::new(move |err: *mut NSError| {
        if let Some(reply) = reply.lock().unwrap().take() {
            if err.is_null() {
                populate_worker_on_success(submit_abstime, &worker);
                let _ = state_tx.send(ok_state);
                let _ = reply.send(Ok(()));
            } else {
                let error = unsafe { &*err };
                let _ = state_tx.send(err_state);
                let _ = reply.send(Err(crate::sys::ns_error_to_error(error)));
            }
        }
    })
}

/// Completion-handler variant that clears the tracked VZ worker on success.
/// Used by `dispatch_stop` (and `dispatch_save_state` if desired — but save
/// keeps the same worker, so it stays on the plain helper).
pub(crate) fn make_stop_like_completion_block(
    reply: Mutex<Option<oneshot::Sender<crate::Result<()>>>>,
    state_tx: watch::Sender<VmState>,
    ok_state: VmState,
    err_state: VmState,
    worker: WorkerSlot,
) -> RcBlock<dyn Fn(*mut NSError)> {
    RcBlock::new(move |err: *mut NSError| {
        if let Some(reply) = reply.lock().unwrap().take() {
            if err.is_null() {
                clear_worker(&worker, "stop completion");
                let _ = state_tx.send(ok_state);
                let _ = reply.send(Ok(()));
            } else {
                let error = unsafe { &*err };
                let _ = state_tx.send(err_state);
                let _ = reply.send(Err(crate::sys::ns_error_to_error(error)));
            }
        }
    })
}

/// Holds all the ObjC state on the dispatch queue.
pub(crate) struct InnerMachine {
    pub(crate) vm: Retained<VZVirtualMachine>,
    pub(crate) _vz_config: Retained<VZVirtualMachineConfiguration>,
    pub(crate) _delegate: Retained<VmDelegate>,
    pub(crate) queue: DispatchRetained<DispatchQueue>,
    pub(crate) state_tx: watch::Sender<VmState>,
    /// The original config as passed by the caller (never mutated).
    pub(crate) config: VmConfig,
    /// A copy of `config` with all auto-generated values (MACs, machine
    /// identifier) resolved from Virtualization.framework.  Use this for
    /// save/restore round-trips.
    pub(crate) snapshot: VmConfig,
    /// PID + start-time of the VZ worker subprocess backing this VM, for
    /// metric sampling via `proc_pid_rusage`. `None` when the VM is not
    /// running (or before the first start completion fires).
    pub(crate) worker: WorkerSlot,
    /// Live `VmnetProxy` instances keeping the vmnet bridge interfaces
    /// up for this VM's NICs. Dropping an `InnerMachine` drops these,
    /// which joins the pumps and calls `vmnet_stop_interface`. Empty for
    /// VMs with no `NetAttachment::Vmnet` NICs.
    pub(crate) network_proxies: NetworkProxies,
}

impl InnerMachine {
    pub(crate) fn new(config: VmConfig) -> crate::Result<Self> {
        let built = build_vz_config(&config)?;
        let vz_config = built.vz_config;
        let network_proxies: NetworkProxies = Arc::new(Mutex::new(built.network_proxies));
        let queue = DispatchQueue::new("com.vfrust.vm", DispatchQueueAttr::SERIAL);

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<DelegateEvent>();
        let delegate = VmDelegate::new(event_tx);

        let vm = unsafe {
            VZVirtualMachine::initWithConfiguration_queue(
                VZVirtualMachine::alloc(),
                &vz_config,
                &queue,
            )
        };

        unsafe {
            vm.setDelegate(Some(delegate.as_protocol_object()));
        }

        let (state_tx, _) = watch::channel(VmState::Stopped);
        let worker: WorkerSlot = Arc::new(Mutex::new(None));

        let state_tx_clone = state_tx.clone();
        let worker_clone = worker.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    DelegateEvent::GuestStopped => {
                        tracing::info!("guest stopped");
                        let _ = state_tx_clone.send(VmState::Stopped);
                        clear_worker(&worker_clone, "guest stopped");
                    }
                    DelegateEvent::Error(msg) => {
                        tracing::error!("VM error: {msg}");
                        let _ = state_tx_clone.send(VmState::Error);
                        clear_worker(&worker_clone, "vm error");
                    }
                    DelegateEvent::NetworkDisconnected(msg) => {
                        tracing::warn!("network disconnected: {msg}");
                    }
                }
            }
        });

        // Build a snapshot config with all auto-generated values resolved.
        // The original `config` stays untouched so it can be reused as a
        // template for spawning additional VMs.
        let snapshot = {
            let mut snap = config.clone();

            // Read back auto-generated MACs from the VZ config.
            // `build_devices` pushes VirtioNet configs in iteration order, so
            // `net_idx` stays in sync with `vz_config.networkDevices()`.
            {
                let net_devices = unsafe { vz_config.networkDevices() };
                let count = net_devices.count();
                let mut net_idx = 0usize;
                for device in &mut snap.devices {
                    if let crate::config::device::Device::VirtioNet(ref mut net) = device {
                        if net.mac_address.is_none() && net_idx < count {
                            let vz_dev = net_devices.objectAtIndex(net_idx);
                            let vz_mac = unsafe { vz_dev.MACAddress() };
                            let mac_str = unsafe { vz_mac.string() }.to_string();
                            if let Some(parsed) = crate::config::device::network::MacAddress::parse(&mac_str) {
                                net.mac_address = Some(parsed);
                            }
                        }
                        net_idx += 1;
                    }
                }
            }

            // Read the machine identifier (Generic platform).
            // Captures both explicit and auto-generated identifiers.
            if snap.machine_identifier.is_none() {
                snap.machine_identifier = unsafe {
                    use objc2_virtualization::{VZGenericPlatformConfiguration, VZGenericMachineIdentifier};
                    let platform = vz_config.platform();
                    let generic: Option<Retained<VZGenericPlatformConfiguration>> =
                        objc2::rc::Retained::downcast(platform).ok();
                    generic.map(|g| {
                        let id: Retained<VZGenericMachineIdentifier> = g.machineIdentifier();
                        let data = id.dataRepresentation();
                        let len = data.length();
                        let mut bytes = vec![0u8; len];
                        if len > 0 {
                            data.getBytes_length(
                                std::ptr::NonNull::new(bytes.as_mut_ptr().cast()).unwrap(),
                                len,
                            );
                        }
                        bytes
                    })
                };
            }

            snap
        };

        Ok(Self {
            vm,
            _vz_config: vz_config,
            _delegate: delegate,
            queue,
            state_tx,
            config,
            snapshot,
            worker,
            network_proxies,
        })
    }

    pub(crate) fn worker(&self) -> WorkerSlot {
        self.worker.clone()
    }

    pub(crate) fn network_proxies(&self) -> NetworkProxies {
        self.network_proxies.clone()
    }

    pub(crate) fn vm_ptr(&self) -> VmPtr {
        VmPtr::new(&self.vm)
    }

    pub(crate) fn dispatch_start(&self, reply: oneshot::Sender<crate::Result<()>>) {
        let vm_ptr = self.vm_ptr();
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Starting);
        let reply = Mutex::new(Some(reply));
        let worker = self.worker.clone();

        self.queue.exec_async(move || {
            // Record host time just before the framework call. Our forked
            // VZ worker must have a strictly greater `proc_start_abstime`.
            let submit_abstime = process_info::mach_absolute_time();
            let block = make_start_like_completion_block(
                reply,
                state_tx,
                VmState::Running,
                VmState::Error,
                submit_abstime,
                worker,
            );
            unsafe { vm_ptr.get().startWithCompletionHandler(&block) };
        });
    }

    pub(crate) fn dispatch_pause(&self, reply: oneshot::Sender<crate::Result<()>>) {
        let vm_ptr = self.vm_ptr();
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Pausing);
        let reply = Mutex::new(Some(reply));

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Paused, VmState::Error)),
            );
            unsafe { vm_ptr.get().pauseWithCompletionHandler(&block) };
        });
    }

    pub(crate) fn dispatch_resume(&self, reply: oneshot::Sender<crate::Result<()>>) {
        let vm_ptr = self.vm_ptr();
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Resuming);
        let reply = Mutex::new(Some(reply));

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Running, VmState::Error)),
            );
            unsafe { vm_ptr.get().resumeWithCompletionHandler(&block) };
        });
    }

    pub(crate) fn dispatch_stop(&self, reply: oneshot::Sender<crate::Result<()>>) {
        let vm_ptr = self.vm_ptr();
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Stopping);
        let reply = Mutex::new(Some(reply));
        let worker = self.worker.clone();

        self.queue.exec_async(move || {
            let block = make_stop_like_completion_block(
                reply,
                state_tx,
                VmState::Stopped,
                VmState::Error,
                worker,
            );
            unsafe { vm_ptr.get().stopWithCompletionHandler(&block) };
        });
    }

    pub(crate) fn dispatch_request_stop(&self, reply: oneshot::Sender<crate::Result<()>>) {
        let vm_ptr = self.vm_ptr();
        // `requestStop` only asks the guest to shut down; the actual
        // stop transition arrives via `DelegateEvent::GuestStopped`,
        // which clears the worker. Nothing to do here on the worker
        // slot — a sync clear would race with an in-flight shutdown.
        self.queue.exec_async(move || {
            let vm = unsafe { vm_ptr.get() };
            let result = unsafe { vm.requestStopWithError() };
            match result {
                Ok(()) => {
                    let _ = reply.send(Ok(()));
                }
                Err(e) => {
                    let _ = reply.send(Err(crate::sys::ns_error_to_error(&e)));
                }
            }
        });
    }

    pub(crate) fn dispatch_save_state(
        &self,
        path: &std::path::Path,
        reply: oneshot::Sender<crate::Result<()>>,
    ) {
        let vm_ptr = self.vm_ptr();
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Saving);
        let reply = Mutex::new(Some(reply));

        let url = match crate::sys::nsurl_from_path(path) {
            Ok(url) => url,
            Err(e) => {
                if let Some(reply) = reply.lock().unwrap().take() {
                    let _ = reply.send(Err(e));
                }
                return;
            }
        };

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Paused, VmState::Error)),
            );
            unsafe {
                vm_ptr
                    .get()
                    .saveMachineStateToURL_completionHandler(&url, &block);
            }
        });
    }

    pub(crate) fn dispatch_restore_state(
        &self,
        path: &std::path::Path,
        reply: oneshot::Sender<crate::Result<()>>,
    ) {
        let vm_ptr = self.vm_ptr();
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Restoring);
        let reply = Mutex::new(Some(reply));
        let worker = self.worker.clone();

        let url = match crate::sys::nsurl_from_path(path) {
            Ok(url) => url,
            Err(e) => {
                if let Some(reply) = reply.lock().unwrap().take() {
                    let _ = reply.send(Err(e));
                }
                return;
            }
        };

        self.queue.exec_async(move || {
            // Restore forks a fresh worker, same as a cold start.
            let submit_abstime = process_info::mach_absolute_time();
            let block = make_start_like_completion_block(
                reply,
                state_tx,
                VmState::Paused,
                VmState::Stopped,
                submit_abstime,
                worker,
            );
            unsafe {
                vm_ptr
                    .get()
                    .restoreMachineStateFromURL_completionHandler(&url, &block);
            }
        });
    }
}
