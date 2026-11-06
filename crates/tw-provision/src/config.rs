use serde::{Deserialize, Serialize};

/// `setup\guest.json` on the answer ISO, read by `install-agent.ps1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestConfig {
    pub vm_name: String,
    pub service_guid: String,
    /// Base64 of the 32-byte channel secret.
    pub channel_secret: String,
    pub agent_version: String,
    pub options: GuestOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestOptions {
    /// `on` keeps Windows' default UAC behaviour; `off` removes the secure-desktop prompts.
    pub uac: String,
    /// `hvsock` (Hyper-V) or `tcp` (the agent listens on a port that the host reaches through a NAT forward).
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default)]
    pub port: Option<u16>,
    /// Blocks the guest's outbound network (TCP channel only; Hyper-V offline VMs have no adapter).
    #[serde(default)]
    pub offline: bool,
}

fn default_channel() -> String {
    "hvsock".into()
}

impl GuestConfig {
    pub fn new(vm_name: &str, service_guid: &str, secret: &[u8], agent_version: &str, uac_off: bool) -> Self {
        use base64::Engine;
        Self {
            vm_name: vm_name.to_string(),
            service_guid: service_guid.to_string(),
            channel_secret: base64::engine::general_purpose::STANDARD.encode(secret),
            agent_version: agent_version.to_string(),
            options: GuestOptions {
                uac: if uac_off { "off" } else { "on" }.into(),
                channel: default_channel(),
                port: None,
                offline: false,
            },
        }
    }

    /// Makes the agent listen on TCP `port` instead of a Hyper-V socket.
    pub fn with_tcp(mut self, port: u16, offline: bool) -> Self {
        self.options.channel = "tcp".into();
        self.options.port = Some(port);
        self.options.offline = offline;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_the_names_the_install_script_reads() {
        let config = GuestConfig::new("default", "b7a4d3c0-5e21-4f8a-9c6d-2e81f0a7d315", &[1u8; 32], "0.1.0", true);
        let json = serde_json::to_value(&config).unwrap();
        for key in ["vmName", "serviceGuid", "channelSecret", "agentVersion"] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["options"]["uac"], "off");
        assert_eq!(GuestConfig::new("a", "b", &[0; 32], "c", false).options.uac, "on");
        assert_eq!(json["channelSecret"].as_str().unwrap().len(), 44);
        assert_eq!(json["options"]["channel"], "hvsock");
    }

    #[test]
    fn the_tcp_channel_carries_its_port_and_offline_flag() {
        let config = GuestConfig::new("a", "b", &[0; 32], "c", false).with_tcp(47831, true);
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!((json["options"]["channel"].as_str(), json["options"]["port"].as_u64()), (Some("tcp"), Some(47831)));
        assert_eq!(json["options"]["offline"], true);
        let old: GuestConfig = serde_json::from_str(
            r#"{"vmName":"a","serviceGuid":"b","channelSecret":"c","agentVersion":"d","options":{"uac":"on"}}"#,
        )
        .unwrap();
        assert_eq!(old.options.channel, "hvsock", "files from older builds still load");
    }
}
