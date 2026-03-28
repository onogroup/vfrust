use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol};
use objc2_virtualization::{VZNetworkDevice, VZVirtualMachine, VZVirtualMachineDelegate};
use tokio::sync::mpsc;

#[derive(Debug)]
pub(crate) enum DelegateEvent {
    GuestStopped,
    Error(String),
    NetworkDisconnected(String),
}

pub(crate) struct DelegateIvars {
    event_tx: RefCell<Option<mpsc::UnboundedSender<DelegateEvent>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VfRustVmDelegate"]
    #[ivars = DelegateIvars]
    pub(crate) struct VmDelegate;

    unsafe impl NSObjectProtocol for VmDelegate {}

    unsafe impl VZVirtualMachineDelegate for VmDelegate {
        #[unsafe(method(guestDidStopVirtualMachine:))]
        fn guest_did_stop(&self, _vm: &VZVirtualMachine) {
            self.send_event(DelegateEvent::GuestStopped);
        }

        #[unsafe(method(virtualMachine:didStopWithError:))]
        fn did_stop_with_error(&self, _vm: &VZVirtualMachine, error: &NSError) {
            let msg = error.to_string();
            self.send_event(DelegateEvent::Error(msg));
        }

        #[unsafe(method(virtualMachine:networkDevice:attachmentWasDisconnectedWithError:))]
        fn network_disconnected(
            &self,
            _vm: &VZVirtualMachine,
            _device: &VZNetworkDevice,
            error: &NSError,
        ) {
            let msg = error.to_string();
            self.send_event(DelegateEvent::NetworkDisconnected(msg));
        }
    }
);

impl VmDelegate {
    pub(crate) fn new(event_tx: mpsc::UnboundedSender<DelegateEvent>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars {
            event_tx: RefCell::new(Some(event_tx)),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn send_event(&self, event: DelegateEvent) {
        if let Some(tx) = self.ivars().event_tx.borrow().as_ref() {
            let _ = tx.send(event);
        }
    }

    pub(crate) fn as_protocol_object(&self) -> &ProtocolObject<dyn VZVirtualMachineDelegate> {
        ProtocolObject::from_ref(self)
    }
}
