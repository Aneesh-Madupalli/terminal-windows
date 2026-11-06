//! The live frame stream: a connection dedicated to `subscribe`, separate from request connections.

use std::collections::BTreeMap;
use std::time::Duration;

use tw_proto::frame::decompress;
use tw_proto::{Header, Limits, ProtoError};
use tw_transport::{Framed, Target, Transport};
use uuid::Uuid;

use crate::{AgentInfo, ClientError, open};

/// One decoded frame: top-down BGRA pixels at `width` x `height`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub screen_width: u32,
    pub screen_height: u32,
    pub dpi: u32,
    pub bgra: Vec<u8>,
}

pub struct FrameStream {
    framed: Framed,
    pub agent: AgentInfo,
}

impl FrameStream {
    pub async fn connect(
        transport: &dyn Transport,
        target: &Target,
        secret: &[u8],
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let (framed, agent, _) = open(transport, target, secret, timeout).await?;
        Ok(Self { framed, agent })
    }

    /// Starts (or resizes) the stream and waits for the agent's reply. The first frame follows at once.
    pub async fn subscribe(
        &mut self,
        max_width: u32,
        max_height: u32,
        fps: u32,
        timeout: Duration,
    ) -> Result<(), ClientError> {
        let id = Uuid::new_v4();
        let args: BTreeMap<String, String> = [("maxWidth", max_width), ("maxHeight", max_height), ("fps", fps)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let request = Header::Req { id, action: "subscribe".into(), args, deadline_ms: timeout.as_millis() as u64 };
        self.framed.write(&request, &[]).await?;
        let wait = async {
            loop {
                let message = self.framed.read(Limits::AUTHENTICATED).await?;
                if let Header::Rep { id: reply_id, ok, values, error } = message.header {
                    if reply_id == id {
                        return Ok::<_, ProtoError>((ok, values, error));
                    }
                }
            }
        };
        let (ok, values, error) = tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| ClientError::Timeout { action: "subscribe".into(), connected: true })??;
        if ok {
            Ok(())
        } else {
            Err(ClientError::Remote { code: values.get("code").cloned(), message: error.unwrap_or_default() })
        }
    }

    /// Waits for the next frame. Cancel-safe, so it can sit in a `select!` next to other work.
    pub async fn next_frame(&mut self) -> Result<Frame, ClientError> {
        loop {
            let message = self.framed.read(Limits::AUTHENTICATED).await?;
            if let Header::Frame { seq, format, width, height, bytes_per_row, screen_width, screen_height, dpi } =
                message.header
            {
                if format != "bgra-lz4" {
                    return Err(ProtoError::InvalidHeader(format!("unknown frame format {format}")).into());
                }
                let bgra = decompress(&message.payload, width, height, bytes_per_row)?;
                return Ok(Frame { seq, width, height, screen_width, screen_height, dpi, bgra });
            }
        }
    }

    /// Tells the agent a frame was handed to the renderer, so it may send another.
    pub async fn ack(&mut self, seq: u64) -> Result<(), ClientError> {
        Ok(self.framed.write(&Header::Ack { seq }, &[]).await?)
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::net::TcpListener;
    use tw_proto::auth::random_bytes;
    use tw_proto::frame::compress;
    use tw_proto::{AgentHandshake, ErrorCode};
    use tw_transport::{BoxStream, LoopbackTransport};

    use super::*;

    const SECRET: [u8; 32] = [4; 32];

    /// An agent that answers `subscribe`, sends two frames, and counts acks.
    async fn agent(refuse: bool) -> (SocketAddr, tokio::sync::mpsc::UnboundedReceiver<u64>) {
        let (acks, received) = tokio::sync::mpsc::unbounded_channel();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(Box::new(stream) as BoxStream);
            let handshake = AgentHandshake::new(&SECRET, "0.1.0", 1, random_bytes());
            framed.write(&handshake.hello(), &[]).await.unwrap();
            let auth = framed.read(Limits::PRE_AUTH).await.unwrap();
            let (confirmation, _) = handshake.verify_auth(&auth.header).unwrap();
            framed.write(&confirmation, &[]).await.unwrap();
            let request = framed.read(Limits::AUTHENTICATED).await.unwrap();
            let Header::Req { id, .. } = request.header else { panic!() };
            if refuse {
                let values = [("code".to_string(), ErrorCode::BadRequest.as_str().to_string())].into();
                framed.write(&Header::Rep { id, ok: false, values, error: Some("no".into()) }, &[]).await.unwrap();
                return;
            }
            framed.write(&Header::Rep { id, ok: true, values: BTreeMap::new(), error: None }, &[]).await.unwrap();
            for seq in 1..=2u8 {
                let pixels = vec![seq; 4 * 2 * 4];
                let header = Header::Frame {
                    seq: u64::from(seq),
                    format: "bgra-lz4".into(),
                    width: 4,
                    height: 2,
                    bytes_per_row: 16,
                    screen_width: 8,
                    screen_height: 4,
                    dpi: 96,
                };
                framed.write(&header, &compress(&pixels)).await.unwrap();
            }
            while let Ok(message) = framed.read(Limits::AUTHENTICATED).await {
                if let Header::Ack { seq } = message.header {
                    acks.send(seq).unwrap();
                }
            }
        });
        (address, received)
    }

    async fn connect(address: SocketAddr) -> FrameStream {
        FrameStream::connect(&LoopbackTransport, &Target::Loopback(address), &SECRET, Duration::from_secs(2))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn frames_are_decoded_and_acknowledged() {
        let (address, mut acks) = agent(false).await;
        let mut stream = connect(address).await;
        assert_eq!(stream.agent.build, 1);
        stream.subscribe(4, 2, 30, Duration::from_secs(2)).await.unwrap();
        let first = stream.next_frame().await.unwrap();
        assert_eq!((first.seq, first.width, first.height, first.screen_width, first.dpi), (1, 4, 2, 8, 96));
        assert_eq!(first.bgra, vec![1u8; 32]);
        stream.ack(first.seq).await.unwrap();
        let second = stream.next_frame().await.unwrap();
        assert_eq!(second.bgra, vec![2u8; 32]);
        stream.ack(second.seq).await.unwrap();
        assert_eq!(acks.recv().await, Some(1));
        assert_eq!(acks.recv().await, Some(2));
    }

    #[tokio::test]
    async fn a_refused_subscription_reports_the_agents_code() {
        let (address, _acks) = agent(true).await;
        let mut stream = connect(address).await;
        let error = stream.subscribe(4, 2, 30, Duration::from_secs(2)).await.unwrap_err();
        assert!(matches!(error, ClientError::Remote { code: Some(ref c), .. } if c == "bad-request"), "{error}");
    }

    #[tokio::test]
    async fn a_closed_connection_ends_the_stream_with_an_error() {
        let (address, _acks) = agent(true).await;
        let mut stream = connect(address).await;
        let _ = stream.subscribe(4, 2, 30, Duration::from_secs(2)).await;
        assert!(stream.next_frame().await.is_err());
    }
}
