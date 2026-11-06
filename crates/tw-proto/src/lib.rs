//! Wire protocol between the host and the guest agent: framing, headers, authentication, actions.

pub mod action;
pub mod auth;
pub mod error;
pub mod frame;
pub mod message;

pub use action::{Action, Button, parse_action};
pub use auth::{AgentHandshake, HostHandshake};
pub use error::{ErrorCode, ProtoError};
pub use message::{Header, Limits, Message, read_message, try_decode, write_message};

/// Fixed for the lifetime of protocol major version 1, so any agent build stays reachable for updates.
pub const SERVICE_GUID: &str = "b7a4d3c0-5e21-4f8a-9c6d-2e81f0a7d315";

/// Port the guest agent listens on when the hypervisor has no Hyper-V sockets (VirtualBox).
pub const AGENT_TCP_PORT: u16 = 47831;

/// Highest protocol version this build speaks.
pub const PROTOCOL_VERSION: u32 = 1;
