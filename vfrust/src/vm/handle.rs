use std::sync::Mutex;

use dispatch2::{DispatchQueue, DispatchRetained};
use tokio::sync::{oneshot, watch};

use crate::config::vm::VmConfig;
use crate::error::Error;
use crate::sys::machine::{
    make_completion_block, make_start_like_completion_block, make_stop_like_completion_block,
    InnerMachine, VmPtr, WorkerSlot,
};
use crate::sys::process_info;
use crate::vm::metrics::ResourceUsage;
use crate::vm::state::VmState;

/// A thread-safe handle to a [`VirtualMachine`](super::machine::VirtualMachine).
///
/// This type is `Send + Sync` and dispatches commands to the VM's
/// serial dispatch queue for execution. State is tracked via a watch
/// channel that is updated by completion blocks on the dispatch queue.
#[derive(Clone)]
pub struct VmHandle {
    queue: DispatchRetained<DispatchQueue>,
    vm_ptr: VmPtr,
    state_tx: watch::Sender<VmState>,
    state_rx: watch::Receiver<VmState>,
    config: VmConfig,
    snapshot: VmConfig,
    worker: WorkerSlot,
}

// Safety: VmHandle only dispatches closures onto the VM's serial queue.
// The VmPtr wraps the pointer as usize and is only dereferenced on the queue.
unsafe impl Send for VmHandle {}
unsafe impl Sync for VmHandle {}

impl VmHandle {
    pub(crate) fn new(inner: &InnerMachine) -> Self {
        Self {
            queue: inner.queue.clone(),
            vm_ptr: inner.vm_ptr(),
            state_tx: inner.state_tx.clone(),
            state_rx: inner.state_tx.subscribe(),
            config: inner.config.clone(),
            snapshot: inner.snapshot.clone(),
            worker: inner.worker(),
        }
    }

    /// Access the VM's dispatch queue (crate-internal).
    pub(crate) fn queue(&self) -> &DispatchRetained<DispatchQueue> {
        &self.queue
    }

    /// Access the VM pointer (crate-internal).
    pub(crate) fn vm_ptr(&self) -> VmPtr {
        self.vm_ptr
    }

    /// Get the current VM state.
    pub fn state(&self) -> VmState {
        *self.state_rx.borrow()
    }

    /// Subscribe to state changes.
    pub fn state_stream(&self) -> watch::Receiver<VmState> {
        self.state_rx.clone()
    }

    /// Get the original configuration (auto-generated values not filled in).
    pub fn config(&self) -> &VmConfig {
        &self.config
    }

    /// Get the resolved configuration with all auto-generated values filled in.
    ///
    /// See [`VirtualMachine::snapshot_config`] for details.
    pub fn snapshot_config(&self) -> &VmConfig {
        &self.snapshot
    }

    /// Start the VM.
    pub async fn start(&self) -> crate::Result<()> {
        let state = self.state();
        if !state.can_start() {
            return Err(Error::InvalidState {
                current: state,
                operation: "start",
            });
        }

        let (tx, rx) = oneshot::channel();
        let vm_ptr = self.vm_ptr;
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Starting);
        let reply = Mutex::new(Some(tx));
        let worker = self.worker.clone();

        self.queue.exec_async(move || {
            // Mark host time right before the framework call so we can
            // disambiguate our freshly-forked VZ worker from any other
            // VZ workers belonging to concurrent starts in this process.
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

        rx.await
            .map_err(|_| Error::DispatchError("channel closed".into()))?
    }

    /// Pause the VM.
    pub async fn pause(&self) -> crate::Result<()> {
        let state = self.state();
        if !state.can_pause() {
            return Err(Error::InvalidState {
                current: state,
                operation: "pause",
            });
        }

        let (tx, rx) = oneshot::channel();
        let vm_ptr = self.vm_ptr;
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Pausing);
        let reply = Mutex::new(Some(tx));

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Paused, VmState::Error)),
            );
            unsafe { vm_ptr.get().pauseWithCompletionHandler(&block) };
        });

        rx.await
            .map_err(|_| Error::DispatchError("channel closed".into()))?
    }

    /// Resume the VM from paused state.
    pub async fn resume(&self) -> crate::Result<()> {
        let state = self.state();
        if !state.can_resume() {
            return Err(Error::InvalidState {
                current: state,
                operation: "resume",
            });
        }

        let (tx, rx) = oneshot::channel();
        let vm_ptr = self.vm_ptr;
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Resuming);
        let reply = Mutex::new(Some(tx));

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Running, VmState::Error)),
            );
            unsafe { vm_ptr.get().resumeWithCompletionHandler(&block) };
        });

        rx.await
            .map_err(|_| Error::DispatchError("channel closed".into()))?
    }

    /// Force-stop the VM immediately.
    pub async fn stop(&self) -> crate::Result<()> {
        let state = self.state();
        if !state.can_stop() {
            return Err(Error::InvalidState {
                current: state,
                operation: "stop",
            });
        }

        let (tx, rx) = oneshot::channel();
        let vm_ptr = self.vm_ptr;
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Stopping);
        let reply = Mutex::new(Some(tx));
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

        rx.await
            .map_err(|_| Error::DispatchError("channel closed".into()))?
    }

    /// Request graceful shutdown.
    pub async fn request_stop(&self) -> crate::Result<()> {
        let state = self.state();
        if !state.can_request_stop() {
            return Err(Error::InvalidState {
                current: state,
                operation: "request_stop",
            });
        }

        let (tx, rx) = oneshot::channel();
        let vm_ptr = self.vm_ptr;

        self.queue.exec_async(move || {
            let vm = unsafe { vm_ptr.get() };
            let result = unsafe { vm.requestStopWithError() };
            match result {
                Ok(()) => {
                    let _ = tx.send(Ok(()));
                }
                Err(e) => {
                    let _ = tx.send(Err(crate::sys::ns_error_to_error(&e)));
                }
            }
        });

        rx.await
            .map_err(|_| Error::DispatchError("channel closed".into()))?
    }

    /// Save VM state to a file (VM must be paused).
    pub async fn save_state(&self, path: &std::path::Path) -> crate::Result<()> {
        let (tx, rx) = oneshot::channel();
        let vm_ptr = self.vm_ptr;
        let url = crate::sys::nsurl_from_path(path)?;
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Saving);
        let reply = Mutex::new(Some(tx));

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

        rx.await
            .map_err(|_| Error::DispatchError("channel closed".into()))?
    }

    /// Restore VM state from a file (VM must be stopped).
    pub async fn restore_state(&self, path: &std::path::Path) -> crate::Result<()> {
        let (tx, rx) = oneshot::channel();
        let vm_ptr = self.vm_ptr;
        let url = crate::sys::nsurl_from_path(path)?;
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Restoring);
        let reply = Mutex::new(Some(tx));
        let worker = self.worker.clone();

        self.queue.exec_async(move || {
            // Restore forks a fresh VZ worker, same as a cold start.
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

        rx.await
            .map_err(|_| Error::DispatchError("channel closed".into()))?
    }

    /// Sample host-observed resource usage of the VZ worker process backing
    /// this VM.
    ///
    /// Returns `None` when the VM is not running, the worker subprocess has
    /// not yet been discovered (brief window at startup), or the underlying
    /// `proc_pid_rusage` syscall fails. The call is sync, non-blocking, and
    /// safe from any thread.
    ///
    /// Reads are guarded against OS PID reuse by verifying that the worker
    /// still has the same `proc_start_abstime` recorded at discovery time
    /// and is still identifiable as a VZ worker. If either check fails the
    /// stored identity is left in place (the next lifecycle event will
    /// clear it) but this call returns `None`.
    pub fn resource_usage(&self) -> Option<ResourceUsage> {
        let id = *self.worker.lock().ok()?.as_ref()?;
        // Strong PID-reuse guard.
        if process_info::proc_start_abstime(id.pid) != Some(id.start_abstime)
            || !process_info::is_vz_worker(id.pid)
        {
            return None;
        }
        let info = process_info::proc_rusage_v4(id.pid)?;
        Some(ResourceUsage {
            sampled_at: std::time::SystemTime::now(),
            cpu_user_ns: info.ri_user_time,
            cpu_system_ns: info.ri_system_time,
            resident_bytes: info.ri_resident_size,
            phys_footprint_bytes: info.ri_phys_footprint,
            peak_phys_footprint_bytes: info.ri_interval_max_phys_footprint,
            wired_bytes: info.ri_wired_size,
            disk_read_bytes: info.ri_diskio_bytesread,
            disk_write_bytes: info.ri_diskio_byteswritten,
            pageins: info.ri_pageins,
            energy_nj: info.ri_billed_energy,
            instructions: info.ri_instructions,
            cycles: info.ri_cycles,
        })
    }

    /// The OS PID of the VZ worker process backing this VM, or `None` when
    /// no worker is active. Useful for cross-referencing with `ps` or
    /// Activity Monitor.
    pub fn worker_pid(&self) -> Option<u32> {
        self.worker.lock().ok()?.as_ref().map(|w| w.pid as u32)
    }
}
