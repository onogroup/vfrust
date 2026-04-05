use std::sync::Mutex;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_foundation::NSError;
use objc2_virtualization::{VZVirtualMachine, VZVirtualMachineConfiguration, VZVirtualMachineState};
use tokio::sync::{mpsc, oneshot, watch};

use crate::config::vm::VmConfig;
use crate::sys::config_builder::build_vz_config;
use crate::sys::delegate::{DelegateEvent, VmDelegate};
use crate::vm::state::VmState;

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
}

impl InnerMachine {
    pub(crate) fn new(config: VmConfig) -> crate::Result<Self> {
        let vz_config = build_vz_config(&config)?;
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

        let state_tx_clone = state_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    DelegateEvent::GuestStopped => {
                        tracing::info!("guest stopped");
                        let _ = state_tx_clone.send(VmState::Stopped);
                    }
                    DelegateEvent::Error(msg) => {
                        tracing::error!("VM error: {msg}");
                        let _ = state_tx_clone.send(VmState::Error);
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
        })
    }

    pub(crate) fn vm_ptr(&self) -> VmPtr {
        VmPtr::new(&self.vm)
    }

    pub(crate) fn dispatch_start(&self, reply: oneshot::Sender<crate::Result<()>>) {
        let vm_ptr = self.vm_ptr();
        let state_tx = self.state_tx.clone();
        let _ = state_tx.send(VmState::Starting);
        let reply = Mutex::new(Some(reply));

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Running, VmState::Error)),
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

        self.queue.exec_async(move || {
            let block = make_completion_block(
                reply,
                Some((state_tx, VmState::Stopped, VmState::Error)),
            );
            unsafe { vm_ptr.get().stopWithCompletionHandler(&block) };
        });
    }

    pub(crate) fn dispatch_request_stop(&self, reply: oneshot::Sender<crate::Result<()>>) {
        let vm_ptr = self.vm_ptr();

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
                Some((state_tx, VmState::Paused, VmState::Stopped)),
            );
            unsafe {
                vm_ptr
                    .get()
                    .restoreMachineStateFromURL_completionHandler(&url, &block);
            }
        });
    }
}
