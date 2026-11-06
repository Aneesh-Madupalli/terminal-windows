//! The guest agent: lets the host see and drive the guest desktop over a Hyper-V socket.

pub mod config;
pub mod desktop;
pub mod guard;
pub mod input;
pub mod log;
#[cfg(windows)]
pub mod registry;
pub mod scale;
pub mod server;
pub mod supervise;
pub mod update;
#[cfg(windows)]
pub mod win;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Integer build number, compared by the update flow. Raise it with every released agent.
pub const BUILD: u32 = 1;
