//! Hyper-V sockets (`AF_HYPERV`): a byte stream between the host and a VM that needs no network adapter.
//! Whether tokio's I/O driver can poll these sockets is checked by spike S5; the connection code below
//! converts them to tokio streams exactly as it would a TCP socket.

use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::io::{FromRawSocket, RawSocket};
use std::ptr::null_mut;
use std::sync::Once;
use std::time::Duration;

use tokio::net::TcpStream;
use uuid::Uuid;
use windows_sys::Win32::Networking::WinSock::{
    INVALID_SOCKET, SOCK_STREAM, SOCKADDR, SOCKET, SOCKET_ERROR, WSADATA, WSAGetLastError, WSAStartup, accept, bind,
    closesocket, connect, listen, socket,
};

use crate::{BoxStream, Target, Transport};

const AF_HYPERV: i32 = 34;
const HV_PROTOCOL_RAW: i32 = 1;

/// Matches any VM; used when listening inside the guest.
const WILDCARD: Uuid = Uuid::nil();

/// `SOCKADDR_HV`. GUIDs are stored in the Windows (little-endian field) byte order.
#[repr(C)]
struct SockAddrHv {
    family: u16,
    reserved: u16,
    vm_id: [u8; 16],
    service_id: [u8; 16],
}

fn address(vm_id: Uuid, service_id: Uuid) -> SockAddrHv {
    SockAddrHv {
        family: AF_HYPERV as u16,
        reserved: 0,
        vm_id: vm_id.to_bytes_le(),
        service_id: service_id.to_bytes_le(),
    }
}

fn last_error() -> io::Error {
    // SAFETY: reads the calling thread's last Winsock error.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

fn new_socket() -> io::Result<SOCKET> {
    static START: Once = Once::new();
    START.call_once(|| {
        // SAFETY: WSAStartup only writes into the zeroed structure it is given.
        unsafe {
            let mut data: WSADATA = zeroed();
            WSAStartup(0x0202, &mut data);
        }
    });
    // SAFETY: plain socket creation.
    let handle = unsafe { socket(AF_HYPERV, SOCK_STREAM, HV_PROTOCOL_RAW) };
    if handle == INVALID_SOCKET { Err(last_error()) } else { Ok(handle) }
}

/// Wraps an owned socket. The std stream owns it from here on and closes it on drop.
fn into_std(handle: SOCKET) -> std::net::TcpStream {
    // SAFETY: `handle` is a valid, owned stream socket that nothing else will close.
    unsafe { std::net::TcpStream::from_raw_socket(handle as RawSocket) }
}

fn into_stream(std: std::net::TcpStream) -> io::Result<BoxStream> {
    std.set_nonblocking(true)?;
    Ok(Box::new(TcpStream::from_std(std)?))
}

/// What a failed connect means for the user, by Winsock error code (TDD section 7.4).
pub fn describe_connect_error(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(10061) => "the VM is running but the guest agent is not listening yet",
        Some(10060) => "the VM is off, starting, or unreachable",
        Some(10054) => "the connection was reset (the VM was paused, restored, or the agent restarted)",
        Some(10013) => "access denied: the host service registration or rights are missing",
        Some(10047) => "this Windows has no Hyper-V socket support",
        _ => "the guest control channel could not be opened",
    }
}

/// Connects from the host to a service listening inside a VM.
pub struct HvSockTransport;

#[async_trait::async_trait]
impl Transport for HvSockTransport {
    async fn connect(&self, target: &Target, timeout: Duration) -> io::Result<BoxStream> {
        let Target::HvSock { vm_id, service_id } = target else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a Hyper-V socket target"));
        };
        let (vm_id, service_id) = (*vm_id, *service_id);
        let attempt = tokio::task::spawn_blocking(move || -> io::Result<std::net::TcpStream> {
            let handle = new_socket()?;
            let target = address(vm_id, service_id);
            // SAFETY: `target` is a valid SOCKADDR_HV of the stated length and outlives the call.
            let result =
                unsafe { connect(handle, (&raw const target).cast::<SOCKADDR>(), size_of::<SockAddrHv>() as i32) };
            if result == SOCKET_ERROR {
                let error = last_error();
                // SAFETY: the socket is ours and unused after this.
                unsafe { closesocket(handle) };
                return Err(error);
            }
            Ok(into_std(handle))
        });
        let std = tokio::time::timeout(timeout, attempt)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))?
            .map_err(io::Error::other)??;
        into_stream(std)
    }
}

/// Listens inside the guest for the host's connection.
pub struct HvSockListener {
    handle: SOCKET,
}

impl HvSockListener {
    pub fn bind(service_id: Uuid) -> io::Result<Self> {
        let handle = new_socket()?;
        let local = address(WILDCARD, service_id);
        // SAFETY: `local` is a valid SOCKADDR_HV of the stated length.
        let bound = unsafe { bind(handle, (&raw const local).cast::<SOCKADDR>(), size_of::<SockAddrHv>() as i32) };
        // SAFETY: the socket is valid.
        if bound == SOCKET_ERROR || unsafe { listen(handle, 8) } == SOCKET_ERROR {
            let error = last_error();
            // SAFETY: the socket is ours and unused after this.
            unsafe { closesocket(handle) };
            return Err(error);
        }
        Ok(Self { handle })
    }

    pub async fn accept(&self) -> io::Result<BoxStream> {
        let handle = self.handle;
        let std = tokio::task::spawn_blocking(move || -> io::Result<std::net::TcpStream> {
            // SAFETY: blocks until a peer connects; closing the listener from another thread makes it fail.
            let connection = unsafe { accept(handle, null_mut(), null_mut()) };
            if connection == INVALID_SOCKET { Err(last_error()) } else { Ok(into_std(connection)) }
        })
        .await
        .map_err(io::Error::other)??;
        into_stream(std)
    }
}

impl Drop for HvSockListener {
    fn drop(&mut self) {
        // SAFETY: the listening socket is owned by this value.
        unsafe { closesocket(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_use_the_windows_byte_order() {
        let id = Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        let a = address(id, id);
        assert_eq!(a.vm_id[..4], [0x33, 0x22, 0x11, 0x00], "the first field is little-endian");
        assert_eq!(a.vm_id[8..], [0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(a.family, 34);
        assert_eq!(size_of::<SockAddrHv>(), 36);
    }

    #[test]
    fn connect_errors_are_explained() {
        for (code, needle) in [(10061, "not listening"), (10060, "off"), (10054, "reset"), (10013, "denied")] {
            assert!(describe_connect_error(&io::Error::from_raw_os_error(code)).contains(needle), "{code}");
        }
        assert!(describe_connect_error(&io::Error::other("x")).contains("could not be opened"));
    }

    /// There is no such VM, so this must fail promptly instead of hanging.
    #[tokio::test]
    async fn connecting_to_a_missing_vm_fails_quickly() {
        let target = Target::HvSock { vm_id: Uuid::from_u128(0x1234), service_id: Uuid::from_u128(0x5678) };
        let started = std::time::Instant::now();
        let result = HvSockTransport.connect(&target, Duration::from_secs(3)).await;
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(8));
    }
}
