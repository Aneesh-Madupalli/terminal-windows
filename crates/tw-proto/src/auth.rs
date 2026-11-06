use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::PROTOCOL_VERSION;
use crate::error::ProtoError;
use crate::message::Header;

pub const SECRET_LEN: usize = 32;
pub const NONCE_LEN: usize = 32;

type HmacSha256 = Hmac<Sha256>;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system random source is available");
    bytes
}

/// `HMAC-SHA256(secret, nonce || label)`; the label stops one side's MAC being replayed as the other's.
fn tag(secret: &[u8], nonce: &[u8], label: &[u8]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(nonce);
    mac.update(label);
    mac
}

fn compute(secret: &[u8], nonce: &[u8], label: &[u8]) -> String {
    STANDARD.encode(tag(secret, nonce, label).finalize().into_bytes())
}

/// Constant-time verification.
fn verify(secret: &[u8], nonce: &[u8], label: &[u8], presented: &str) -> Result<(), ProtoError> {
    let presented = STANDARD.decode(presented).map_err(|_| ProtoError::AuthFailed)?;
    tag(secret, nonce, label).verify_slice(&presented).map_err(|_| ProtoError::AuthFailed)
}

fn decode_nonce(encoded: &str) -> Result<[u8; NONCE_LEN], ProtoError> {
    STANDARD
        .decode(encoded)
        .ok()
        .and_then(|bytes| <[u8; NONCE_LEN]>::try_from(bytes).ok())
        .ok_or_else(|| ProtoError::InvalidHeader("nonce must be 32 bytes".into()))
}

const HOST_LABEL: &[u8] = b"host";
const AGENT_LABEL: &[u8] = b"agent";

/// The guest side: sends `hello`, then checks the host's `auth` and answers `auth-ok`.
pub struct AgentHandshake {
    secret: Vec<u8>,
    agent_version: String,
    build: u32,
    nonce: [u8; NONCE_LEN],
}

impl AgentHandshake {
    pub fn new(secret: &[u8], agent_version: &str, build: u32, nonce: [u8; NONCE_LEN]) -> Self {
        Self { secret: secret.to_vec(), agent_version: agent_version.to_string(), build, nonce }
    }

    pub fn hello(&self) -> Header {
        Header::Hello {
            proto: PROTOCOL_VERSION,
            agent: self.agent_version.clone(),
            build: self.build,
            nonce: STANDARD.encode(self.nonce),
        }
    }

    /// Returns the `auth-ok` reply and the negotiated protocol version.
    pub fn verify_auth(&self, message: &Header) -> Result<(Header, u32), ProtoError> {
        let Header::Auth { proto, mac, client_nonce } = message else {
            return Err(ProtoError::Unexpected("auth"));
        };
        let client_nonce = decode_nonce(client_nonce)?;
        verify(&self.secret, &self.nonce, HOST_LABEL, mac)?;
        let negotiated = (*proto).min(PROTOCOL_VERSION);
        if negotiated < 1 {
            return Err(ProtoError::UnsupportedProtocol);
        }
        let reply = Header::AuthOk { mac: compute(&self.secret, &client_nonce, AGENT_LABEL) };
        Ok((reply, negotiated))
    }
}

/// The host side: answers the agent's `hello`, then checks the agent's `auth-ok`.
pub struct HostHandshake {
    secret: Vec<u8>,
    client_nonce: [u8; NONCE_LEN],
    negotiated: Option<u32>,
}

impl HostHandshake {
    pub fn new(secret: &[u8], client_nonce: [u8; NONCE_LEN]) -> Self {
        Self { secret: secret.to_vec(), client_nonce, negotiated: None }
    }

    pub fn on_hello(&mut self, message: &Header) -> Result<Header, ProtoError> {
        let Header::Hello { proto, nonce, .. } = message else {
            return Err(ProtoError::Unexpected("hello"));
        };
        let agent_nonce = decode_nonce(nonce)?;
        let negotiated = (*proto).min(PROTOCOL_VERSION);
        if negotiated < 1 {
            return Err(ProtoError::UnsupportedProtocol);
        }
        self.negotiated = Some(negotiated);
        Ok(Header::Auth {
            proto: PROTOCOL_VERSION,
            mac: compute(&self.secret, &agent_nonce, HOST_LABEL),
            client_nonce: STANDARD.encode(self.client_nonce),
        })
    }

    /// Returns the negotiated protocol version once the agent proved it knows the secret.
    pub fn on_auth_ok(&self, message: &Header) -> Result<u32, ProtoError> {
        let Header::AuthOk { mac } = message else {
            return Err(ProtoError::Unexpected("auth-ok"));
        };
        let negotiated = self.negotiated.ok_or(ProtoError::Unexpected("hello first"))?;
        verify(&self.secret, &self.client_nonce, AGENT_LABEL, mac)?;
        Ok(negotiated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; SECRET_LEN] = [7; SECRET_LEN];

    fn pair(agent_secret: &[u8], host_secret: &[u8]) -> (AgentHandshake, HostHandshake) {
        (AgentHandshake::new(agent_secret, "0.1.0", 1, [1; NONCE_LEN]), HostHandshake::new(host_secret, [2; NONCE_LEN]))
    }

    #[test]
    fn full_handshake_succeeds() {
        let (agent, mut host) = pair(&SECRET, &SECRET);
        let auth = host.on_hello(&agent.hello()).unwrap();
        let (ok, agent_proto) = agent.verify_auth(&auth).unwrap();
        assert_eq!(host.on_auth_ok(&ok).unwrap(), PROTOCOL_VERSION);
        assert_eq!(agent_proto, PROTOCOL_VERSION);
    }

    #[test]
    fn wrong_secret_fails_on_the_agent() {
        let (agent, mut host) = pair(&SECRET, &[9; SECRET_LEN]);
        let auth = host.on_hello(&agent.hello()).unwrap();
        assert!(matches!(agent.verify_auth(&auth), Err(ProtoError::AuthFailed)));
    }

    #[test]
    fn impostor_agent_fails_on_the_host() {
        let (_real, mut host) = pair(&SECRET, &SECRET);
        let impostor = AgentHandshake::new(&[9; SECRET_LEN], "0.1.0", 1, [1; NONCE_LEN]);
        let auth = host.on_hello(&impostor.hello()).unwrap();
        // The impostor cannot verify the host, so it answers with a MAC made from its own secret.
        let forged = Header::AuthOk { mac: compute(&[9; SECRET_LEN], &[2; NONCE_LEN], AGENT_LABEL) };
        let _ = auth;
        assert!(matches!(host.on_auth_ok(&forged), Err(ProtoError::AuthFailed)));
    }

    #[test]
    fn a_reflected_host_mac_is_rejected() {
        let (agent, mut host) = pair(&SECRET, &SECRET);
        let auth = host.on_hello(&agent.hello()).unwrap();
        let Header::Auth { mac, .. } = &auth else { unreachable!() };
        let reflected = Header::AuthOk { mac: mac.clone() };
        assert!(matches!(host.on_auth_ok(&reflected), Err(ProtoError::AuthFailed)));
    }

    #[test]
    fn a_mac_for_another_nonce_is_rejected() {
        let (agent, mut host) = pair(&SECRET, &SECRET);
        let _ = host.on_hello(&agent.hello()).unwrap();
        // A MAC captured from an earlier session (different agent nonce) is useless now.
        let old = AgentHandshake::new(&SECRET, "0.1.0", 1, [5; NONCE_LEN]);
        let mut old_host = HostHandshake::new(&SECRET, [6; NONCE_LEN]);
        let old_auth = old_host.on_hello(&old.hello()).unwrap();
        assert!(matches!(agent.verify_auth(&old_auth), Err(ProtoError::AuthFailed)));
    }

    #[test]
    fn protocol_zero_is_unsupported() {
        let (_agent, mut host) = pair(&SECRET, &SECRET);
        let hello = Header::Hello { proto: 0, agent: "old".into(), build: 1, nonce: STANDARD.encode([1u8; NONCE_LEN]) };
        assert!(matches!(host.on_hello(&hello), Err(ProtoError::UnsupportedProtocol)));
    }

    #[test]
    fn a_newer_peer_negotiates_down() {
        let (agent, mut host) = pair(&SECRET, &SECRET);
        let Header::Auth { mac, client_nonce, .. } = host.on_hello(&agent.hello()).unwrap() else { unreachable!() };
        let newer = Header::Auth { proto: 9, mac, client_nonce };
        assert_eq!(agent.verify_auth(&newer).unwrap().1, PROTOCOL_VERSION);
    }

    #[test]
    fn wrong_message_types_are_rejected() {
        let (agent, mut host) = pair(&SECRET, &SECRET);
        assert!(matches!(host.on_hello(&Header::Ack { seq: 1 }), Err(ProtoError::Unexpected(_))));
        assert!(matches!(agent.verify_auth(&Header::Ack { seq: 1 }), Err(ProtoError::Unexpected(_))));
        assert!(matches!(host.on_auth_ok(&Header::Ack { seq: 1 }), Err(ProtoError::Unexpected(_))));
        // auth-ok before hello
        let ok = Header::AuthOk { mac: String::new() };
        assert!(host.on_auth_ok(&ok).is_err());
    }

    #[test]
    fn malformed_nonces_and_macs_fail_cleanly() {
        let (agent, mut host) = pair(&SECRET, &SECRET);
        let short = Header::Hello { proto: 1, agent: "a".into(), build: 1, nonce: STANDARD.encode([1u8; 8]) };
        assert!(host.on_hello(&short).is_err());
        let bad =
            Header::Auth { proto: 1, mac: "!!!not base64".into(), client_nonce: STANDARD.encode([1u8; NONCE_LEN]) };
        assert!(matches!(agent.verify_auth(&bad), Err(ProtoError::AuthFailed)));
    }

    #[test]
    fn random_bytes_differ() {
        assert_ne!(random_bytes::<NONCE_LEN>(), random_bytes::<NONCE_LEN>());
    }
}
