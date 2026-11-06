//! Byte-stream transports between the host and the guest agent, plus message framing over a stream.

pub mod auto;
pub mod framed;
#[cfg(windows)]
pub mod hvsock;
pub mod loopback;

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use uuid::Uuid;

pub use auto::AutoTransport;
pub use framed::Framed;
#[cfg(windows)]
pub use hvsock::{HvSockListener, HvSockTransport};
pub use loopback::LoopbackTransport;

/// A boxed stream keeps `Transport` object-safe, so clients can take `&dyn Transport`.
pub trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncStream for T {}
pub type BoxStream = Box<dyn AsyncStream>;

/// Where to connect. A Hyper-V socket names the VM and the service registered on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Loopback(SocketAddr),
    HvSock { vm_id: Uuid, service_id: Uuid },
}

#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn connect(&self, target: &Target, timeout: Duration) -> io::Result<BoxStream>;
}
