use std::collections::BTreeMap;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ProtoError;

pub const MAGIC: [u8; 4] = *b"TWPR";
const PREFIX: usize = 12;

pub const MAX_HEADER: usize = 262_144;
pub const MAX_PAYLOAD: usize = 67_108_864;

/// Size limits for one message. Before authentication only a tiny header and no payload are accepted,
/// so an unauthenticated peer cannot make the receiver buffer large messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_header: usize,
    pub max_payload: usize,
}

impl Limits {
    pub const PRE_AUTH: Limits = Limits { max_header: 4096, max_payload: 0 };
    pub const AUTHENTICATED: Limits = Limits { max_header: MAX_HEADER, max_payload: MAX_PAYLOAD };
}

/// Message header, a JSON object tagged by `t` (TDD sections 8.2 and 8.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum Header {
    Hello {
        proto: u32,
        agent: String,
        build: u32,
        nonce: String,
    },
    Auth {
        proto: u32,
        mac: String,
        #[serde(rename = "clientNonce")]
        client_nonce: String,
    },
    AuthOk {
        mac: String,
    },
    Req {
        id: Uuid,
        action: String,
        #[serde(default)]
        args: BTreeMap<String, String>,
        #[serde(rename = "deadlineMs")]
        deadline_ms: u64,
    },
    Rep {
        id: Uuid,
        ok: bool,
        #[serde(default)]
        values: BTreeMap<String, String>,
        #[serde(default)]
        error: Option<String>,
    },
    Frame {
        seq: u64,
        format: String,
        width: u32,
        height: u32,
        #[serde(rename = "bytesPerRow")]
        bytes_per_row: u32,
        #[serde(rename = "screenWidth")]
        screen_width: u32,
        #[serde(rename = "screenHeight")]
        screen_height: u32,
        dpi: u32,
    },
    Ack {
        seq: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub header: Header,
    pub payload: Vec<u8>,
}

pub fn encode(header: &Header, payload: &[u8]) -> Result<Vec<u8>, ProtoError> {
    let header = serde_json::to_vec(header).map_err(|e| ProtoError::InvalidHeader(e.to_string()))?;
    if header.len() > MAX_HEADER {
        return Err(ProtoError::HeaderTooLarge(header.len()));
    }
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtoError::PayloadTooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(PREFIX + header.len() + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
    Ok(out)
}

pub fn write_message<W: Write>(writer: &mut W, header: &Header, payload: &[u8]) -> Result<(), ProtoError> {
    writer.write_all(&encode(header, payload)?)?;
    writer.flush()?;
    Ok(())
}

/// Checks the 12-byte prefix and returns `(header_len, payload_len)`.
fn parse_prefix(prefix: &[u8; PREFIX], limits: Limits) -> Result<(usize, usize), ProtoError> {
    if prefix[..4] != MAGIC {
        return Err(ProtoError::BadMagic);
    }
    let header = u32::from_le_bytes(prefix[4..8].try_into().expect("4 bytes")) as usize;
    let payload = u32::from_le_bytes(prefix[8..12].try_into().expect("4 bytes")) as usize;
    if header > limits.max_header {
        return Err(ProtoError::HeaderTooLarge(header));
    }
    if payload > limits.max_payload {
        return Err(ProtoError::PayloadTooLarge(payload));
    }
    Ok((header, payload))
}

fn parse_header(bytes: &[u8]) -> Result<Header, ProtoError> {
    serde_json::from_slice(bytes).map_err(|e| ProtoError::InvalidHeader(e.to_string()))
}

/// Decodes one message from the front of `buffer`. `Ok(None)` means more bytes are needed;
/// otherwise the message and the number of bytes it used are returned. Limits are checked
/// before any large allocation, and a wrong magic fails as soon as the first bytes disagree.
pub fn try_decode(buffer: &[u8], limits: Limits) -> Result<Option<(Message, usize)>, ProtoError> {
    let known = buffer.len().min(MAGIC.len());
    if buffer[..known] != MAGIC[..known] {
        return Err(ProtoError::BadMagic);
    }
    let Some(prefix) = buffer.get(..PREFIX) else { return Ok(None) };
    let (header_len, payload_len) = parse_prefix(prefix.try_into().expect("12 bytes"), limits)?;
    let total = PREFIX + header_len + payload_len;
    if buffer.len() < total {
        return Ok(None);
    }
    let header = parse_header(&buffer[PREFIX..PREFIX + header_len])?;
    let payload = buffer[PREFIX + header_len..total].to_vec();
    Ok(Some((Message { header, payload }, total)))
}

pub fn read_message<R: Read>(reader: &mut R, limits: Limits) -> Result<Message, ProtoError> {
    let mut prefix = [0u8; PREFIX];
    reader.read_exact(&mut prefix)?;
    let (header_len, payload_len) = parse_prefix(&prefix, limits)?;
    let mut header = vec![0u8; header_len];
    reader.read_exact(&mut header)?;
    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload)?;
    Ok(Message { header: parse_header(&header)?, payload })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Header {
        Header::Req {
            id: Uuid::nil(),
            action: "click".into(),
            args: BTreeMap::from([("x".into(), "640".into()), ("y".into(), "400".into())]),
            deadline_ms: 5000,
        }
    }

    #[test]
    fn round_trips_with_payload() {
        let bytes = encode(&request(), b"png-bytes").unwrap();
        let (message, used) = try_decode(&bytes, Limits::AUTHENTICATED).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(message.header, request());
        assert_eq!(message.payload, b"png-bytes");

        let mut cursor = std::io::Cursor::new(bytes);
        assert_eq!(read_message(&mut cursor, Limits::AUTHENTICATED).unwrap(), message);
    }

    #[test]
    fn header_json_matches_the_documented_shape() {
        let json = serde_json::to_value(request()).unwrap();
        assert_eq!(json["t"], "req");
        assert_eq!(json["deadlineMs"], 5000);
        let frame = serde_json::to_value(Header::Frame {
            seq: 1,
            format: "bgra-lz4".into(),
            width: 1280,
            height: 800,
            bytes_per_row: 5120,
            screen_width: 1280,
            screen_height: 800,
            dpi: 96,
        })
        .unwrap();
        assert_eq!(frame["t"], "frame");
        assert_eq!(frame["bytesPerRow"], 5120);
        assert_eq!(frame["screenWidth"], 1280);
        assert_eq!(serde_json::to_value(Header::AuthOk { mac: "x".into() }).unwrap()["t"], "auth-ok");
    }

    #[test]
    fn partial_input_needs_more_bytes_at_every_boundary() {
        let bytes = encode(&request(), b"0123456789").unwrap();
        for cut in 0..bytes.len() {
            assert!(matches!(try_decode(&bytes[..cut], Limits::AUTHENTICATED), Ok(None)), "cut={cut}");
        }
        assert!(try_decode(&bytes, Limits::AUTHENTICATED).unwrap().is_some());
    }

    #[test]
    fn two_messages_in_one_buffer() {
        let mut bytes = encode(&request(), b"a").unwrap();
        let second = encode(&Header::Ack { seq: 7 }, b"").unwrap();
        bytes.extend_from_slice(&second);
        let (first, used) = try_decode(&bytes, Limits::AUTHENTICATED).unwrap().unwrap();
        assert_eq!(first.payload, b"a");
        let (next, _) = try_decode(&bytes[used..], Limits::AUTHENTICATED).unwrap().unwrap();
        assert_eq!(next.header, Header::Ack { seq: 7 });
    }

    #[test]
    fn rejects_wrong_magic_immediately() {
        assert!(matches!(try_decode(b"X", Limits::AUTHENTICATED), Err(ProtoError::BadMagic)));
        assert!(matches!(try_decode(b"TWPX", Limits::AUTHENTICATED), Err(ProtoError::BadMagic)));
        assert!(matches!(try_decode(b"TW", Limits::AUTHENTICATED), Ok(None)));
        assert!(matches!(try_decode(b"", Limits::AUTHENTICATED), Ok(None)));
    }

    fn prefix(header: u32, payload: u32) -> Vec<u8> {
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&header.to_le_bytes());
        v.extend_from_slice(&payload.to_le_bytes());
        v
    }

    #[test]
    fn rejects_oversized_lengths_before_allocating() {
        let huge_header = prefix(MAX_HEADER as u32 + 1, 0);
        assert!(matches!(try_decode(&huge_header, Limits::AUTHENTICATED), Err(ProtoError::HeaderTooLarge(_))));
        let huge_payload = prefix(2, MAX_PAYLOAD as u32 + 1);
        assert!(matches!(try_decode(&huge_payload, Limits::AUTHENTICATED), Err(ProtoError::PayloadTooLarge(_))));
        let mut cursor = std::io::Cursor::new(prefix(u32::MAX, u32::MAX));
        assert!(matches!(read_message(&mut cursor, Limits::AUTHENTICATED), Err(ProtoError::HeaderTooLarge(_))));
    }

    #[test]
    fn pre_auth_limits_refuse_payloads_and_large_headers() {
        let with_payload = encode(&request(), b"x").unwrap();
        assert!(matches!(try_decode(&with_payload, Limits::PRE_AUTH), Err(ProtoError::PayloadTooLarge(1))));
        let big_header = prefix(4097, 0);
        assert!(matches!(try_decode(&big_header, Limits::PRE_AUTH), Err(ProtoError::HeaderTooLarge(4097))));
        let ok = encode(&Header::Ack { seq: 1 }, b"").unwrap();
        assert!(try_decode(&ok, Limits::PRE_AUTH).unwrap().is_some());
    }

    #[test]
    fn rejects_garbage_headers() {
        let mut bytes = prefix(4, 0);
        bytes.extend_from_slice(b"nope");
        assert!(matches!(try_decode(&bytes, Limits::AUTHENTICATED), Err(ProtoError::InvalidHeader(_))));
        let mut unknown = prefix(13, 0);
        unknown.extend_from_slice(br#"{"t":"bogus"}"#);
        assert!(matches!(try_decode(&unknown, Limits::AUTHENTICATED), Err(ProtoError::InvalidHeader(_))));
    }

    #[test]
    fn truncated_stream_is_an_io_error() {
        let bytes = encode(&request(), b"abc").unwrap();
        let mut cursor = std::io::Cursor::new(bytes[..bytes.len() - 1].to_vec());
        assert!(matches!(read_message(&mut cursor, Limits::AUTHENTICATED), Err(ProtoError::Io(_))));
    }

    #[test]
    fn encode_rejects_oversized_payload() {
        let big = vec![0u8; MAX_PAYLOAD + 1];
        assert!(matches!(encode(&Header::Ack { seq: 0 }, &big), Err(ProtoError::PayloadTooLarge(_))));
    }
}
