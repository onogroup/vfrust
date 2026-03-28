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

    /// Get the configuration this VM was created with.
    pub fn config(&self) -> &VmConfig {
        &self.inner.config
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
}

impl Drop for VirtualMachine {
    fn drop(&mut self) {
        // Clear the delegate on the VM's dispatch queue to prevent callbacks
        // to a dropped delegate object. The Retained<VZVirtualMachine> will
        // be released when InnerMachine drops, which is safe from any thread
        // for ref-counted ObjC objects.
        let vm_ptr = self.inner.vm_ptr();
        self.inner.queue.exec_async(move || unsafe {
            vm_ptr.get().setDelegate(None);
        });
    }
}
