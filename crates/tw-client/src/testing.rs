//! A fake guest agent for the tests of this crate.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use tokio::net::TcpListener;
use tw_proto::auth::random_bytes;
use tw_proto::{Action, AgentHandshake, Header, Limits, parse_action};
use tw_transport::{BoxStream, Framed};

pub const SECRET: [u8; 32] = [7; 32];

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Normal,
    /// Closes each connection after answering one request.
    HangUpAfterOne,
}

fn reply(id: uuid::Uuid, values: &[(&str, &str)], error: Option<&str>) -> Header {
    let values: BTreeMap<String, String> = values.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    Header::Rep { id, ok: error.is_none(), values, error: error.map(str::to_string) }
}

/// Serves connections one after another on a free loopback port.
pub async fn start_agent(mode: Mode) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            tokio::spawn(async move {
                let mut framed = Framed::new(Box::new(stream) as BoxStream);
                let handshake = AgentHandshake::new(&SECRET, "0.1.0", 1, random_bytes());
                if framed.write(&handshake.hello(), &[]).await.is_err() {
                    return;
                }
                let Ok(auth) = framed.read(Limits::PRE_AUTH).await else { return };
                let Ok((confirmation, _)) = handshake.verify_auth(&auth.header) else { return };
                if framed.write(&confirmation, &[]).await.is_err() {
                    return;
                }
                while let Ok(message) = framed.read(Limits::AUTHENTICATED).await {
                    let Header::Req { id, action, args, .. } = message.header else { return };
                    let header = match parse_action(&action, &args, Some((1280, 800))) {
                        Ok(Action::Status) => reply(id, &[("session", "ready")], None),
                        Ok(_) => reply(id, &[], None),
                        Err(e) => reply(id, &[("code", "bad-request")], Some(&e.to_string())),
                    };
                    if framed.write(&header, &[]).await.is_err() || mode == Mode::HangUpAfterOne {
                        return;
                    }
                }
            });
        }
    });
    address
}
