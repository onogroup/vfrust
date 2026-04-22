use std::sync::Mutex;

use block2::RcBlock;

use crate::config::vm::VmConfig;
use crate::sys::machine::InnerMachine;
use crate::vm::handle::VmHandle;
use crate::vm::state::VmState;

/// A virtual machine instance.
///
/// This type is `!Send` because it wraps `VZVirtualMachine` which must
/// remain on its dispatch queue. Use [`VmHandle`] for cross-thread access.
pub struct VirtualMachine {
    pub(crate) inner: InnerMachine,
}

impl VirtualMachine {
    /// Create a new VM from a validated configuration.
    pub fn new(config: VmConfig) -> crate::Result<Self> {
        let inner = InnerMachine::new(config)?;
        Ok(Self { inner })
    }

    /// Get the current VM state.
    pub fn state(&self) -> VmState {
        *self.inner.state_tx.borrow()
    }

    /// Get the original configuration this VM was created with.
    ///
    /// Auto-generated values (MACs, machine identifier) are **not** filled in.
    /// Use [`snapshot_config`](Self::snapshot_config) for a config suitable for
    /// save/restore round-trips.
    pub fn config(&self) -> &VmConfig {
        &self.inner.config
    }

    /// Get the resolved configuration with all auto-generated values filled in.
    ///
    /// This includes MAC addresses and the Generic-platform machine identifier
    /// that Virtualization.framework assigned at creation time.  Persist this
    /// (via [`VmConfig::to_json`]) alongside save files so the restore VM can
    /// be created with matching identity.
    pub fn snapshot_config(&self) -> &VmConfig {
        &self.inner.snapshot
    }

    /// Create a thread-safe handle for cross-thread VM control.
    pub fn handle(&self) -> VmHandle {
        VmHandle::new(&self.inner)
    }

    /// Start the VM.
    pub async fn start(&self) -> crate::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner.dispatch_start(tx);
        rx.await.map_err(|_| crate::Error::DispatchError("channel closed".into()))?
    }

    /// Pause the VM.
    pub async fn pause(&self) -> crate::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner.dispatch_pause(tx);
        rx.await.map_err(|_| crate::Error::DispatchError("channel closed".into()))?
    }

    /// Resume the VM from paused state.
    pub async fn resume(&self) -> crate::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner.dispatch_resume(tx);
        rx.await.map_err(|_| crate::Error::DispatchError("channel closed".into()))?
    }

    /// Force-stop the VM immediately.
    pub async fn stop(&self) -> crate::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner.dispatch_stop(tx);
        rx.await.map_err(|_| crate::Error::DispatchError("channel closed".into()))?
    }

    /// Request a graceful stop (sends ACPI shutdown signal to guest).
    pub async fn request_stop(&self) -> crate::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner.dispatch_request_stop(tx);
        rx.await.map_err(|_| crate::Error::DispatchError("channel closed".into()))?
    }

    /// Save VM state to a file (VM must be paused).
    pub async fn save_state(&self, path: &std::path::Path) -> crate::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner.dispatch_save_state(path, tx);
        rx.await.map_err(|_| crate::Error::DispatchError("channel closed".into()))?
    }

    /// Restore VM state from a file (VM must be stopped).
    pub async fn restore_state(&self, path: &std::path::Path) -> crate::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner.dispatch_restore_state(path, tx);
        rx.await.map_err(|_| crate::Error::DispatchError("channel closed".into()))?
    }

    /// Sample host-observed resource usage of the VZ worker process.
    ///
    /// See [`VmHandle::resource_usage`] for semantics.
    pub fn resource_usage(&self) -> Option<crate::vm::metrics::ResourceUsage> {
        self.handle().resource_usage()
    }

    /// OS PID of the VZ worker process backing this VM, or `None` when
    /// no worker is active.
    pub fn worker_pid(&self) -> Option<u32> {
        self.handle().worker_pid()
    }
}

/// Drop force-stops a running VM and clears the ObjC delegate before
/// deallocating.  This blocks the current thread for up to 7 seconds
/// (5s stop + 2s delegate clear).  Callers in async contexts should
/// call [`VirtualMachine::stop`] explicitly before dropping.
impl Drop for VirtualMachine {
    fn drop(&mut self) {
        let vm_ptr = self.inner.vm_ptr();
        let state = *self.inner.state_tx.borrow();

        // If the VM is running, paused, or in an error state, force-stop it
        // synchronously before dropping. This prevents the ObjC
        // VZVirtualMachine from being deallocated while still active.
        if state.can_stop() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.inner.queue.exec_async(move || {
                let reply = Mutex::new(Some(tx));
                let block = RcBlock::new(move |err: *mut objc2_foundation::NSError| {
                    if let Some(tx) = reply.lock().unwrap().take() {
                        if err.is_null() {
                            let _ = tx.send(Ok(()));
                        } else {
                            let _ = tx.send(Err(()));
                        }
                    }
                });
                unsafe { vm_ptr.get().stopWithCompletionHandler(&block) };
            });
            // Wait up to 5s for the stop to complete.
            let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
        }

        // Clear the delegate to prevent callbacks to dropped objects.
        let vm_ptr = self.inner.vm_ptr();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        self.inner.queue.exec_async(move || {
            unsafe { vm_ptr.get().setDelegate(None) };
            let _ = done_tx.send(());
        });
        // Wait for delegate clear to complete before InnerMachine drops.
        let _ = done_rx.recv_timeout(std::time::Duration::from_secs(2));
    }
}
