use std::io;
use std::time::Duration;

use tokio::net::TcpStream;

use crate::{BoxStream, Target, Transport};

/// TCP on 127.0.0.1, used by tests and as a development stand-in for the Hyper-V socket.
pub struct LoopbackTransport;

#[async_trait::async_trait]
impl Transport for LoopbackTransport {
    async fn connect(&self, target: &Target, timeout: Duration) -> io::Result<BoxStream> {
        let Target::Loopback(address) = target else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a loopback target"));
        };
        let stream = tokio::time::timeout(timeout, TcpStream::connect(address))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;
        stream.set_nodelay(true)?;
        Ok(Box::new(stream))
    }
}
