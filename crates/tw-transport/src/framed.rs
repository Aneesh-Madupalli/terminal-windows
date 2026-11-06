use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tw_proto::message::encode;
use tw_proto::{Header, Limits, Message, ProtoError, try_decode};

use crate::BoxStream;

/// Messages over a byte stream. Bytes are buffered until a whole message is available,
/// and the size limits are checked before any large allocation.
pub struct Framed {
    stream: BoxStream,
    buffer: Vec<u8>,
}

impl Framed {
    pub fn new(stream: BoxStream) -> Self {
        Self { stream, buffer: Vec::new() }
    }

    pub async fn read(&mut self, limits: Limits) -> Result<Message, ProtoError> {
        loop {
            if let Some((message, used)) = try_decode(&self.buffer, limits)? {
                self.buffer.drain(..used);
                return Ok(message);
            }
            let mut chunk = [0u8; 16 * 1024];
            let count = self.stream.read(&mut chunk).await?;
            if count == 0 {
                return Err(ProtoError::Io(io::ErrorKind::UnexpectedEof.into()));
            }
            self.buffer.extend_from_slice(&chunk[..count]);
        }
    }

    pub async fn write(&mut self, header: &Header, payload: &[u8]) -> Result<(), ProtoError> {
        let bytes = encode(header, payload)?;
        self.stream.write_all(&bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }
}
