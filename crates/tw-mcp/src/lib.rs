//! A Model Context Protocol server over stdio that gives agents computer-use tools for the guest desktop.

pub mod image;
pub mod server;
pub mod tools;

pub use server::McpServer;
