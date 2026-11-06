//! Terminal plumbing that needs no operating system: input parsing, size queries, layout,
//! chrome drawing, graphics encoders and protocol detection.

pub mod bitmap;
pub mod capability;
pub mod chrome;
pub mod input;
pub mod layout;
pub mod metrics;
pub mod render;
pub mod writer;

pub use bitmap::Bitmap;
pub use capability::Protocol;
pub use input::{TerminalEvent, TerminalInput};
pub use layout::{DesktopPlacement, ImageLayout};
pub use metrics::TerminalMetricFilter;
pub use writer::TerminalWriter;
