use std::sync::Mutex;

use dispatch2::{DispatchQueue, DispatchRetained};
use tokio::sync::{oneshot, watch};

use crate::config::vm::VmConfig;
use crate::error::Error;
use crate::sys::machine::{make_completion_block, InnerMachine, VmPtr};
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

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Running, VmState::Error)),
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

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Stopped, VmState::Error)),
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

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Paused, VmState::Stopped)),
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
}
