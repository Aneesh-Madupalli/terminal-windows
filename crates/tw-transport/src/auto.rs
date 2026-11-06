use std::io;
use std::time::Duration;

use crate::{BoxStream, LoopbackTransport, Target, Transport};

/// Picks the transport from the target: Hyper-V sockets for Hyper-V guests, TCP for forwarded ports.
pub struct AutoTransport;

#[async_trait::async_trait]
impl Transport for AutoTransport {
    async fn connect(&self, target: &Target, timeout: Duration) -> io::Result<BoxStream> {
        match target {
            Target::Loopback(_) => LoopbackTransport.connect(target, timeout).await,
            #[cfg(windows)]
            Target::HvSock { .. } => crate::HvSockTransport.connect(target, timeout).await,
            #[cfg(not(windows))]
            Target::HvSock { .. } => Err(io::Error::new(io::ErrorKind::Unsupported, "Hyper-V sockets need Windows")),
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn loopback_targets_connect_over_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"x").await.unwrap();
        });
        assert!(AutoTransport.connect(&Target::Loopback(address), Duration::from_secs(2)).await.is_ok());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn hv_socket_targets_are_routed_to_hyper_v() {
        let target = Target::HvSock { vm_id: uuid::Uuid::nil(), service_id: uuid::Uuid::nil() };
        // No such VM: the error comes from the Hyper-V socket layer, not from routing.
        let error = AutoTransport.connect(&target, Duration::from_millis(500)).await.err().unwrap();
        assert_ne!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
    }
}
