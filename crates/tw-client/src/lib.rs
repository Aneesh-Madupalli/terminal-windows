//! Host-side client for the guest agent: connect, authenticate, and call actions.

pub mod guest;
pub mod live;
pub mod stream;
#[cfg(test)]
mod testing;

pub use guest::{Guest, GuestError};
pub use live::LiveGuest;
pub use stream::{Frame, FrameStream};

use std::collections::BTreeMap;
use std::time::Duration;

use thiserror::Error;
use tokio::sync::Mutex;
use tw_proto::auth::random_bytes;
use tw_proto::{Header, HostHandshake, Limits, ProtoError};
use tw_transport::{Framed, Target, Transport};
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ClientError {
    /// The channel could not be opened: the VM is off, or the agent is not listening.
    #[error("cannot reach the guest agent: {0}")]
    Connect(#[source] std::io::Error),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    /// `connected` is false only when no connection was ever made.
    #[error(
        "timed out waiting for {action}. Ensure the VM is running and the guest agent is running in its signed-in desktop"
    )]
    Timeout { action: String, connected: bool },
    /// The agent answered with an error. `code` is the stable machine code, such as `secure-desktop`.
    #[error("{message}")]
    Remote { code: Option<String>, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub version: String,
    pub build: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub values: BTreeMap<String, String>,
    pub payload: Vec<u8>,
}

/// Connects and authenticates; shared by request connections and frame streams.
pub(crate) async fn open(
    transport: &dyn Transport,
    target: &Target,
    secret: &[u8],
    timeout: Duration,
) -> Result<(Framed, AgentInfo, u32), ClientError> {
    let stream = transport.connect(target, timeout).await.map_err(ClientError::Connect)?;
    let mut framed = Framed::new(stream);
    let handshake = async {
        let hello = framed.read(Limits::PRE_AUTH).await?;
        let Header::Hello { agent, build, .. } = &hello.header else {
            return Err(ProtoError::Unexpected("hello"));
        };
        let mut host = HostHandshake::new(secret, random_bytes());
        framed.write(&host.on_hello(&hello.header)?, &[]).await?;
        let confirmation = framed.read(Limits::PRE_AUTH).await?;
        let protocol = host.on_auth_ok(&confirmation.header)?;
        Ok((AgentInfo { version: agent.clone(), build: *build }, protocol))
    };
    let (agent, protocol) = tokio::time::timeout(timeout, handshake)
        .await
        .map_err(|_| ClientError::Timeout { action: "the handshake".into(), connected: true })??;
    Ok((framed, agent, protocol))
}

pub struct GuestClient {
    framed: Mutex<Framed>,
    pub agent: AgentInfo,
    pub protocol: u32,
}

impl GuestClient {
    /// Opens the channel and proves both sides know the secret before any request is sent.
    pub async fn connect(
        transport: &dyn Transport,
        target: &Target,
        secret: &[u8],
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let (framed, agent, protocol) = open(transport, target, secret, timeout).await?;
        Ok(Self { framed: Mutex::new(framed), agent, protocol })
    }

    /// Sends one request and waits for its reply. Replies to earlier, timed-out requests are skipped.
    pub async fn call(
        &self,
        action: &str,
        args: BTreeMap<String, String>,
        deadline: Duration,
    ) -> Result<Reply, ClientError> {
        self.call_with_payload(action, args, &[], deadline).await
    }

    /// Like `call`, with a payload sent along with the request (used by `update`).
    pub async fn call_with_payload(
        &self,
        action: &str,
        args: BTreeMap<String, String>,
        payload: &[u8],
        deadline: Duration,
    ) -> Result<Reply, ClientError> {
        let id = Uuid::new_v4();
        let request = Header::Req { id, action: action.to_string(), args, deadline_ms: deadline.as_millis() as u64 };
        let mut framed = self.framed.lock().await;
        let exchange = async {
            framed.write(&request, payload).await?;
            loop {
                let message = framed.read(Limits::AUTHENTICATED).await?;
                match message.header {
                    Header::Rep { id: reply_id, ok, values, error } if reply_id == id => {
                        return Ok((ok, values, error, message.payload));
                    }
                    Header::Rep { .. } => continue,
                    _ => return Err(ProtoError::Unexpected("rep")),
                }
            }
        };
        let (ok, values, error, payload) = tokio::time::timeout(deadline, exchange)
            .await
            .map_err(|_| ClientError::Timeout { action: action.to_string(), connected: true })??;
        if ok {
            Ok(Reply { values, payload })
        } else {
            Err(ClientError::Remote {
                code: values.get("code").cloned(),
                message: error.unwrap_or_else(|| "Action failed".into()),
            })
        }
    }
}
