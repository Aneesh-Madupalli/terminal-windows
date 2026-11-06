//! OS-independent core: errors, names, argument parsing, machine records, key table.

pub mod args;
pub mod error;
pub mod keys;
pub mod machine;
pub mod mouse;
pub mod name;
pub mod plan;
pub mod readiness;
pub mod secret;

pub use args::Arguments;
pub use error::{Error, Result};
pub use machine::{HostInfo, Machine, Store, VmOptions};
pub use name::{hv_name, validate_name};
pub use plan::SetupPlan;
pub use readiness::GuestReadiness;
