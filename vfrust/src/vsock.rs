//! Vsock proxy: host-to-guest connections and guest-to-host listeners.
//!
//! The [`VsockConnection`] type wraps a file descriptor obtained from the
//! Virtualization framework's `VZVirtioSocketConnection`. The FD is `dup()`-ed
//! so it outlives the ObjC connection object and can be used from any thread
//! with standard POSIX `read`/`write`.
//!
//! # Connecting (host -> guest)
//!
//! ```no_run
//! # async fn example(handle: &vfrust::VmHandle) -> vfrust::Result<()> {
//! let conn = vfrust::vsock::connect_vsock(handle, 1024).await?;
//! conn.write(b"hello")?;
//! let mut buf = [0u8; 256];
//! let n = conn.read(&mut buf)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Listening (guest -> host)
//!
//! ```no_run
//! # async fn example(handle: &vfrust::VmHandle) -> vfrust::Result<()> {
//! let mut rx = vfrust::vsock::listen_vsock(handle, 1024).await?;
//! while let Some(conn) = rx.recv().await {
//!     // handle incoming connection
//! }
//! # Ok(())
//! # }
//! ```

use std::cell::RefCell;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};
use std::sync::Mutex;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol};
use objc2_virtualization::{
    VZSocketDevice, VZVirtioSocketConnection, VZVirtioSocketDevice, VZVirtioSocketListener,
    VZVirtioSocketListenerDelegate,
};
use tokio::sync::{mpsc, oneshot};

use crate::error::Error;
use crate::vm::handle::VmHandle;

// ---------------------------------------------------------------------------
// VsockConnection
// ---------------------------------------------------------------------------

/// A vsock connection to/from the guest.
///
/// The underlying file descriptor can be used for I/O with standard POSIX
/// `read`/`write` from any thread. The FD is owned by this struct and will
/// be closed on drop.
pub struct VsockConnection {
    fd: OwnedFd,
    source_port: u32,
    destination_port: u32,
}

impl VsockConnection {
    /// Borrow the underlying file descriptor.
    pub fn fd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.fd.as_fd()
    }

    /// The source (host-side) port number.
    pub fn source_port(&self) -> u32 {
        self.source_port
    }

    /// The destination (guest-side) port number.
    pub fn destination_port(&self) -> u32 {
        self.destination_port
    }

    /// Read from the vsock connection.
    ///
    /// This is a blocking POSIX read on the file descriptor.
    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::fd::AsRawFd;
        let n = unsafe {
            libc::read(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    /// Write to the vsock connection.
    ///
    /// This is a blocking POSIX write on the file descriptor.
    pub fn write(&self, buf: &[u8]) -> std::io::Result<usize> {
        use std::os::fd::AsRawFd;
        let n = unsafe {
            libc::write(
                self.fd.as_raw_fd(),
                buf.as_ptr() as *const libc::c_void,
                buf.len(),
            )
        };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

// Safety: The OwnedFd is a plain file descriptor that can be used from any thread.
unsafe impl Send for VsockConnection {}
unsafe impl Sync for VsockConnection {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Duplicate a file descriptor so the resulting `OwnedFd` is independent
/// of the original `VZVirtioSocketConnection` lifetime.
fn dup_connection_fd(connection: &VZVirtioSocketConnection) -> crate::Result<(OwnedFd, u32, u32)> {
    let raw_fd = unsafe { connection.fileDescriptor() };
    if raw_fd < 0 {
        return Err(Error::DispatchError(
            "vsock connection has invalid file descriptor (-1)".into(),
        ));
    }
    let duped = unsafe { libc::dup(raw_fd) };
    if duped < 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    let source_port = unsafe { connection.sourcePort() };
    let destination_port = unsafe { connection.destinationPort() };
    let owned = unsafe { OwnedFd::from_raw_fd(duped) };
    Ok((owned, source_port, destination_port))
}

/// Downcast the first `VZSocketDevice` from `socketDevices()` to
/// `VZVirtioSocketDevice`.
///
/// # Safety
/// Must be called on the VM's dispatch queue while the VM is alive.
unsafe fn get_first_vsock_device(
    vm: &objc2_virtualization::VZVirtualMachine,
) -> crate::Result<Retained<VZVirtioSocketDevice>> {
    let devices: Retained<objc2_foundation::NSArray<VZSocketDevice>> = vm.socketDevices();
    let count = devices.count();
    if count == 0 {
        return Err(Error::InvalidDevice(
            "VM has no socket devices configured".into(),
        ));
    }
    let device: Retained<VZSocketDevice> = devices.objectAtIndex(0);

    // The device is actually a VZVirtioSocketDevice (subclass of VZSocketDevice)
    // when created from VZVirtioSocketDeviceConfiguration. We use cast_unchecked
    // since the device was created from VZVirtioSocketDeviceConfiguration.
    Ok(Retained::cast_unchecked(device))
}

// ---------------------------------------------------------------------------
// connect_vsock  (host -> guest)
// ---------------------------------------------------------------------------

/// Connect to a vsock port on the guest.
///
/// The VM must be started and must have a `VirtioVsock` device configured.
/// The call dispatches onto the VM's serial queue internally.
pub async fn connect_vsock(handle: &VmHandle, port: u32) -> crate::Result<VsockConnection> {
    let (tx, rx) = oneshot::channel::<crate::Result<VsockConnection>>();
    let reply = Mutex::new(Some(tx));
    let vm_ptr = handle.vm_ptr();

    handle.queue().exec_async(move || {
        // Safety: we are on the VM's dispatch queue and the VM is alive
        // (guaranteed by VmHandle holding a clone of the queue and pointer).
        let vm = unsafe { vm_ptr.get() };

        let socket_device = match unsafe { get_first_vsock_device(vm) } {
            Ok(d) => d,
            Err(e) => {
                if let Some(reply) = reply.lock().unwrap().take() {
                    let _ = reply.send(Err(e));
                }
                return;
            }
        };

        let block = RcBlock::new(
            move |conn_ptr: *mut VZVirtioSocketConnection, err_ptr: *mut NSError| {
                let result = if !err_ptr.is_null() {
                    let error = unsafe { &*err_ptr };
                    Err(crate::sys::ns_error_to_error(error))
                } else if conn_ptr.is_null() {
                    Err(Error::DispatchError(
                        "vsock connectToPort returned nil connection without error".into(),
                    ))
                } else {
                    let connection = unsafe { &*conn_ptr };
                    match dup_connection_fd(connection) {
                        Ok((fd, src, dst)) => Ok(VsockConnection {
                            fd,
                            source_port: src,
                            destination_port: dst,
                        }),
                        Err(e) => Err(e),
                    }
                };

                if let Some(reply) = reply.lock().unwrap().take() {
                    let _ = reply.send(result);
                }
            },
        );

        unsafe {
            socket_device.connectToPort_completionHandler(port, &block);
        }
    });

    rx.await
        .map_err(|_| Error::DispatchError("vsock connect channel closed".into()))?
}

// ---------------------------------------------------------------------------
// VsockListenerDelegate  (guest -> host)
// ---------------------------------------------------------------------------

/// Internal ivars for the listener delegate.
pub(crate) struct ListenerDelegateIvars {
    conn_tx: RefCell<Option<mpsc::Sender<VsockConnection>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VfRustVsockListenerDelegate"]
    #[ivars = ListenerDelegateIvars]
    pub(crate) struct VsockListenerDelegate;

    unsafe impl NSObjectProtocol for VsockListenerDelegate {}

    unsafe impl VZVirtioSocketListenerDelegate for VsockListenerDelegate {
        #[unsafe(method(listener:shouldAcceptNewConnection:fromSocketDevice:))]
        fn listener_should_accept(
            &self,
            _listener: &VZVirtioSocketListener,
            connection: &VZVirtioSocketConnection,
            _socket_device: &VZVirtioSocketDevice,
        ) -> Bool {
            let tx = match self.ivars().conn_tx.borrow().as_ref() {
                Some(tx) => tx.clone(),
                None => return Bool::NO,
            };

            match dup_connection_fd(connection) {
                Ok((fd, src, dst)) => {
                    let conn = VsockConnection {
                        fd,
                        source_port: src,
                        destination_port: dst,
                    };
                    // Use try_send to avoid blocking the dispatch queue.
                    match tx.try_send(conn) {
                        Ok(()) => Bool::YES,
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            tracing::warn!(
                                "vsock listener channel full, dropping incoming connection"
                            );
                            Bool::NO
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            tracing::debug!("vsock listener channel closed");
                            Bool::NO
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("failed to dup vsock connection fd: {e}");
                    Bool::NO
                }
            }
        }
    }
);

impl VsockListenerDelegate {
    fn new(conn_tx: mpsc::Sender<VsockConnection>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ListenerDelegateIvars {
            conn_tx: RefCell::new(Some(conn_tx)),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn as_protocol_object(&self) -> &ProtocolObject<dyn VZVirtioSocketListenerDelegate> {
        ProtocolObject::from_ref(self)
    }
}

// ---------------------------------------------------------------------------
// listen_vsock  (guest -> host)
// ---------------------------------------------------------------------------

/// The default channel capacity for the listener's connection channel.
const LISTEN_CHANNEL_CAPACITY: usize = 64;

/// Listen for incoming vsock connections on the given port.
///
/// Returns a receiver that yields [`VsockConnection`] values as the guest
/// connects. The VM must be started and must have a `VirtioVsock` device
/// configured.
///
/// The listener remains active until the returned receiver is dropped.
/// Dropping the receiver causes the delegate to reject new connections;
/// the `VZVirtioSocketListener` is not automatically removed from the device
/// (it is benign to leave it registered).
pub async fn listen_vsock(
    handle: &VmHandle,
    port: u32,
) -> crate::Result<mpsc::Receiver<VsockConnection>> {
    let (conn_tx, conn_rx) = mpsc::channel::<VsockConnection>(LISTEN_CHANNEL_CAPACITY);

    let (setup_tx, setup_rx) = oneshot::channel::<crate::Result<()>>();
    let setup_reply = Mutex::new(Some(setup_tx));
    let vm_ptr = handle.vm_ptr();

    handle.queue().exec_async(move || {
        let vm = unsafe { vm_ptr.get() };

        let socket_device = match unsafe { get_first_vsock_device(vm) } {
            Ok(d) => d,
            Err(e) => {
                if let Some(reply) = setup_reply.lock().unwrap().take() {
                    let _ = reply.send(Err(e));
                }
                return;
            }
        };

        // Create the delegate and listener on the dispatch queue (where the
        // ObjC objects must live).
        let delegate = VsockListenerDelegate::new(conn_tx);
        let listener = unsafe { VZVirtioSocketListener::new() };
        unsafe {
            listener.setDelegate(Some(delegate.as_protocol_object()));
        }

        // Register the listener for the requested port.
        unsafe {
            socket_device.setSocketListener_forPort(&listener, port);
        }

        // Keep the delegate and listener alive by leaking Retained references.
        // They will live as long as the socket device holds a reference to the
        // listener. This is intentional: the listener is removed when the VM
        // stops (the socket device is destroyed).
        //
        // NOTE: A more sophisticated implementation could return a guard that
        // calls removeSocketListenerForPort on drop, but for the common use
        // case (listener lives for the VM's lifetime) this is sufficient.
        std::mem::forget(delegate);
        std::mem::forget(listener);

        if let Some(reply) = setup_reply.lock().unwrap().take() {
            let _ = reply.send(Ok(()));
        }
    });

    setup_rx
        .await
        .map_err(|_| Error::DispatchError("vsock listen setup channel closed".into()))??;

    Ok(conn_rx)
}
