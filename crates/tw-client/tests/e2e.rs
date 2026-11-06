//! End-to-end protocol tests: a real `GuestClient` against a fake agent over loopback TCP.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpListener;
use tw_client::{ClientError, GuestClient};
use tw_proto::auth::random_bytes;
use tw_proto::{Action, AgentHandshake, Header, Limits, parse_action};
use tw_transport::{BoxStream, Framed, LoopbackTransport, Target, Transport};

const SECRET: [u8; 32] = [7; 32];
const DISPLAY: (u32, u32) = (1280, 800);
const SHORT: Duration = Duration::from_millis(300);

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

fn reply(id: uuid::Uuid, values: &[(&str, &str)], error: Option<&str>) -> Header {
    Header::Rep { id, ok: error.is_none(), values: args(values), error: error.map(str::to_string) }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    /// Authenticates, then never answers.
    Silent,
    /// Answers every request twice: first with a stale id.
    Stale,
}

async fn start_agent(secret: [u8; 32], mode: Mode) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(Box::new(stream) as BoxStream);
        let handshake = AgentHandshake::new(&secret, "0.1.0", 7, random_bytes());
        framed.write(&handshake.hello(), &[]).await.unwrap();
        let Ok(auth) = framed.read(Limits::PRE_AUTH).await else { return };
        let Ok((confirmation, _)) = handshake.verify_auth(&auth.header) else { return };
        framed.write(&confirmation, &[]).await.unwrap();
        loop {
            let Ok(message) = framed.read(Limits::AUTHENTICATED).await else { return };
            let Header::Req { id, action, args, .. } = message.header else { return };
            if mode == Mode::Silent {
                continue;
            }
            if mode == Mode::Stale {
                framed.write(&reply(uuid::Uuid::new_v4(), &[("stale", "true")], None), &[]).await.unwrap();
            }
            let (header, payload) = match parse_action(&action, &args, Some(DISPLAY)) {
                Ok(Action::Status) => {
                    (reply(id, &[("session", "ready"), ("width", "1280"), ("height", "800")], None), vec![])
                }
                Ok(Action::Screenshot) => (reply(id, &[("width", "1280")], None), b"\x89PNG-fake".to_vec()),
                Ok(_) => (reply(id, &[], None), vec![]),
                Err(e) => (reply(id, &[("code", "bad-request")], Some(&e.to_string())), vec![]),
            };
            framed.write(&header, &payload).await.unwrap();
        }
    });
    address
}

async fn connect(address: SocketAddr, secret: &[u8]) -> Result<GuestClient, ClientError> {
    GuestClient::connect(&LoopbackTransport, &Target::Loopback(address), secret, Duration::from_secs(2)).await
}

#[tokio::test]
async fn status_round_trip() {
    let client = connect(start_agent(SECRET, Mode::Normal).await, &SECRET).await.unwrap();
    assert_eq!(client.agent.version, "0.1.0");
    assert_eq!(client.agent.build, 7);
    assert_eq!(client.protocol, 1);
    let reply = client.call("status", BTreeMap::new(), Duration::from_secs(2)).await.unwrap();
    assert_eq!(reply.values["session"], "ready");
}

#[tokio::test]
async fn screenshot_returns_the_payload() {
    let client = connect(start_agent(SECRET, Mode::Normal).await, &SECRET).await.unwrap();
    let reply = client.call("screenshot", BTreeMap::new(), Duration::from_secs(2)).await.unwrap();
    assert_eq!(reply.payload, b"\x89PNG-fake");
}

#[tokio::test]
async fn several_calls_on_one_connection() {
    let client = connect(start_agent(SECRET, Mode::Normal).await, &SECRET).await.unwrap();
    for _ in 0..5 {
        client.call("click", args(&[("x", "10"), ("y", "20")]), Duration::from_secs(2)).await.unwrap();
    }
}

#[tokio::test]
async fn invalid_requests_come_back_as_remote_errors_with_a_code() {
    let client = connect(start_agent(SECRET, Mode::Normal).await, &SECRET).await.unwrap();
    let error = client.call("click", args(&[("x", "5000"), ("y", "0")]), Duration::from_secs(2)).await.unwrap_err();
    match error {
        ClientError::Remote { code, message } => {
            assert_eq!(code.as_deref(), Some("bad-request"));
            assert!(message.contains("Invalid x"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    // The connection is still usable after a rejected request.
    client.call("status", BTreeMap::new(), Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn a_wrong_secret_never_authenticates() {
    let address = start_agent(SECRET, Mode::Normal).await;
    assert!(connect(address, &[9; 32]).await.is_err());
}

struct Refusing;

#[async_trait::async_trait]
impl Transport for Refusing {
    async fn connect(&self, _: &Target, _: Duration) -> std::io::Result<BoxStream> {
        Err(std::io::ErrorKind::ConnectionRefused.into())
    }
}

#[tokio::test]
async fn nothing_listening_is_a_connect_error() {
    let target = Target::Loopback("127.0.0.1:9".parse().unwrap());
    let result = GuestClient::connect(&Refusing, &target, &SECRET, SHORT).await;
    assert!(matches!(result, Err(ClientError::Connect(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused));
}

#[tokio::test]
async fn a_silent_agent_times_out_after_connecting() {
    let client = connect(start_agent(SECRET, Mode::Silent).await, &SECRET).await.unwrap();
    let error = client.call("status", BTreeMap::new(), SHORT).await.unwrap_err();
    assert!(matches!(error, ClientError::Timeout { connected: true, .. }), "{error:?}");
}

#[tokio::test]
async fn stale_replies_are_skipped() {
    let client = connect(start_agent(SECRET, Mode::Stale).await, &SECRET).await.unwrap();
    let reply = client.call("status", BTreeMap::new(), Duration::from_secs(2)).await.unwrap();
    assert_eq!(reply.values["session"], "ready");
    assert!(!reply.values.contains_key("stale"));
}
