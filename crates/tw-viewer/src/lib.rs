//! The terminal viewer: shows the guest desktop in a terminal and forwards keyboard and mouse input.

pub mod draw;
pub mod mapping;
pub mod palette;
pub mod session;
pub mod terminal;
pub mod view;

pub use session::{LiveSession, Session};
pub use terminal::Terminal;
#[cfg(windows)]
pub use terminal::WindowsTerminal;
pub use view::{Exit, Options, run};
