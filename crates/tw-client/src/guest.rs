use std::collections::BTreeMap;
use std::time::Duration;

use thiserror::Error;

use crate::{ClientError, Reply};

/// What the MCP server, the CLI and the viewer need from a guest: run an action, or press the
/// secure attention sequence through the hypervisor.
#[async_trait::async_trait]
pub trait Guest: Send + Sync {
    async fn call(&self, action: &str, args: BTreeMap<String, String>, timeout: Duration) -> Result<Reply, GuestError>;
    async fn ctrl_alt_del(&self) -> Result<(), GuestError>;

    /// An action that carries a payload, such as `update`.
    async fn upload(
        &self,
        _action: &str,
        _args: BTreeMap<String, String>,
        _payload: Vec<u8>,
        _timeout: Duration,
    ) -> Result<Reply, GuestError> {
        Err(GuestError::Other("This guest does not accept uploads".into()))
    }
}

#[derive(Debug, Error)]
pub enum GuestError {
    #[error("VM '{0}' does not exist")]
    NotCreated(String),
    #[error("VM '{name}' is {state}")]
    NotRunning { name: String, state: String },
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("{0}")]
    Other(String),
}

impl GuestError {
    /// A sentence an agent can relay to the user: what happened and what to do next.
    pub fn explain(&self, name: &str) -> String {
        let up = if name == "default" { String::new() } else { format!(" {name}") };
        match self {
            GuestError::NotCreated(_) => format!(
                "No virtual Windows PC named '{name}' exists yet. Ask the user to run `terminal-windows up{up} --iso PATH`; it installs Windows and needs a Windows ISO."
            ),
            GuestError::NotRunning { state, .. } if state == "paused" => {
                format!("VM '{name}' is paused. Ask the user to run: terminal-windows resume{up}")
            }
            GuestError::NotRunning { state, .. } => {
                format!("VM '{name}' is {state}. Ask the user to run: terminal-windows start{up}")
            }
            GuestError::Client(ClientError::Connect(_)) => format!(
                "The virtual Windows PC has no running guest agent. It is probably still starting or at its sign-in screen. Ask the user to run `terminal-windows view{up}` to check."
            ),
            GuestError::Client(ClientError::Timeout { .. }) => {
                "The guest agent did not answer in time. The guest may be busy, or the agent may have stopped; ask the user to check it with `terminal-windows view`.".into()
            }
            GuestError::Client(ClientError::Remote { code: Some(code), message }) => match code.as_str() {
                "locked" => "The guest is locked. Ask the user to sign in (terminal-windows view, then Ctrl+O).".into(),
                "secure-desktop" => {
                    "A UAC prompt or the sign-in screen is showing and cannot be controlled. Ask the user to handle it.".into()
                }
                "no-session" => "Nobody is signed in to the guest. Ask the user to sign in.".into(),
                _ => message.clone(),
            },
            other => other.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explanations_tell_the_user_what_to_do() {
        let missing = GuestError::NotCreated("work".into()).explain("work");
        assert!(missing.contains("terminal-windows up work --iso PATH"), "{missing}");
        let stopped = GuestError::NotRunning { name: "default".into(), state: "stopped".into() }.explain("default");
        assert!(stopped.contains("terminal-windows start") && !stopped.contains("start default"), "{stopped}");
        let paused = GuestError::NotRunning { name: "d".into(), state: "paused".into() }.explain("d");
        assert!(paused.contains("resume d"), "{paused}");
    }

    #[test]
    fn remote_codes_become_guidance() {
        let locked = GuestError::Client(ClientError::Remote { code: Some("locked".into()), message: "x".into() });
        assert!(locked.explain("default").contains("sign in"));
        let secure =
            GuestError::Client(ClientError::Remote { code: Some("secure-desktop".into()), message: "x".into() });
        assert!(secure.explain("default").contains("UAC"));
        let other =
            GuestError::Client(ClientError::Remote { code: Some("bad-request".into()), message: "Invalid x".into() });
        assert_eq!(other.explain("default"), "Invalid x");
    }

    #[test]
    fn connection_failures_point_at_the_agent() {
        let refused = GuestError::Client(ClientError::Connect(std::io::ErrorKind::ConnectionRefused.into()));
        assert!(refused.explain("default").contains("guest agent"));
        let timeout = GuestError::Client(ClientError::Timeout { action: "status".into(), connected: true });
        assert!(timeout.explain("default").contains("did not answer"));
    }
}
