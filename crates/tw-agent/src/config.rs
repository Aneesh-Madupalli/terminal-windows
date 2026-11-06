//! `C:\ProgramData\terminal-windows\agent.json`, written by the guest install script.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use tw_core::{Error, Result, secret};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    #[serde(default)]
    service_guid: Option<String>,
    /// Base64 of the DPAPI-protected channel secret.
    secret: String,
    #[serde(default)]
    options: Options,
}

#[derive(Debug, Default, Deserialize)]
struct Options {
    #[serde(default)]
    uac: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    port: Option<u16>,
}

/// How the host reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    HvSock,
    Tcp(u16),
}

#[derive(Debug)]
pub struct AgentConfig {
    pub service_id: Uuid,
    pub secret: Vec<u8>,
    pub uac_off: bool,
    pub channel: Channel,
}

/// Parses the file text. `unprotect` turns the stored blob into the secret, so tests need no DPAPI.
pub fn parse(text: &str, unprotect: impl Fn(&[u8]) -> Result<Vec<u8>>) -> Result<AgentConfig> {
    // Windows PowerShell writes UTF-8 with a byte order mark.
    let text = text.trim_start_matches('\u{feff}');
    let file: File = serde_json::from_str(text).map_err(|e| Error::Other(format!("agent.json is invalid: {e}")))?;
    let service_id = match file.service_guid {
        Some(guid) => {
            Uuid::parse_str(&guid).map_err(|_| Error::Other("agent.json has an invalid serviceGuid".into()))?
        }
        None => Uuid::parse_str(tw_proto::SERVICE_GUID).expect("the constant is a valid GUID"),
    };
    let blob =
        STANDARD.decode(file.secret.trim()).map_err(|_| Error::Other("agent.json has an invalid secret".into()))?;
    let secret = unprotect(&blob)?;
    if secret.len() != secret::SECRET_LEN {
        return Err(Error::Other("The channel secret in agent.json is damaged".into()));
    }
    let channel = match file.options.channel.as_deref() {
        None | Some("hvsock") => Channel::HvSock,
        Some("tcp") => Channel::Tcp(file.options.port.unwrap_or(tw_proto::AGENT_TCP_PORT)),
        Some(other) => return Err(Error::Other(format!("agent.json has an unknown channel '{other}'"))),
    };
    Ok(AgentConfig { service_id, secret, uac_off: file.options.uac.as_deref() == Some("off"), channel })
}

pub fn load(path: &Path) -> Result<AgentConfig> {
    let text =
        std::fs::read_to_string(path).map_err(|e| Error::Other(format!("Cannot read {}: {e}", path.display())))?;
    parse(&text, secret::unprotect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(secret: &[u8], extra: &str) -> String {
        format!("{{\"secret\":\"{}\"{extra}}}", STANDARD.encode(secret))
    }

    fn identity(blob: &[u8]) -> Result<Vec<u8>> {
        Ok(blob.to_vec())
    }

    #[test]
    fn parses_a_minimal_file_with_defaults() {
        let config = parse(&file(&[1; 32], ""), identity).unwrap();
        assert_eq!(config.secret, vec![1; 32]);
        assert_eq!(config.service_id.to_string(), tw_proto::SERVICE_GUID);
        assert!(!config.uac_off);
        assert_eq!(config.channel, Channel::HvSock);
    }

    #[test]
    fn the_tcp_channel_uses_its_port_or_the_default() {
        let with_port = parse(&file(&[1; 32], ",\"options\":{\"channel\":\"tcp\",\"port\":50000}"), identity).unwrap();
        assert_eq!(with_port.channel, Channel::Tcp(50000));
        let without = parse(&file(&[1; 32], ",\"options\":{\"channel\":\"tcp\"}"), identity).unwrap();
        assert_eq!(without.channel, Channel::Tcp(tw_proto::AGENT_TCP_PORT));
        assert!(parse(&file(&[1; 32], ",\"options\":{\"channel\":\"pigeon\"}"), identity).is_err());
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let text = format!(
            "\u{feff}{}",
            file(&[2; 32], ",\"serviceGuid\":\"11111111-2222-3333-4444-555555555555\",\"options\":{\"uac\":\"off\"}")
        );
        let config = parse(&text, identity).unwrap();
        assert_eq!(config.service_id.to_string(), "11111111-2222-3333-4444-555555555555");
        assert!(config.uac_off);
    }

    #[test]
    fn damaged_files_are_rejected() {
        assert!(parse("not json", identity).is_err());
        assert!(parse(&file(&[1; 31], ""), identity).is_err(), "wrong secret length");
        assert!(parse("{\"secret\":\"!!!\"}", identity).is_err());
        assert!(parse(&file(&[1; 32], ",\"serviceGuid\":\"nope\""), identity).is_err());
        assert!(parse(&file(&[1; 32], ""), |_| Err(Error::Other("wrong user".into()))).is_err());
    }

    #[test]
    fn load_reads_a_protected_file_like_the_installer_writes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        let blob = secret::protect(&[9; 32]).unwrap();
        std::fs::write(&path, format!("\u{feff}{}", file(&blob, ""))).unwrap();
        assert_eq!(load(&path).unwrap().secret, vec![9; 32]);
        assert!(load(&dir.path().join("missing.json")).is_err());
    }
}
