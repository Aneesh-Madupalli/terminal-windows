//! Everything needed to go from a Windows ISO to an unattended install: reading the ISO, choosing an image,
//! generating the answer file and the answer ISO, and storing the guest account's credentials.

pub mod answer;
pub mod config;
pub mod credential;
pub mod iso;
pub mod wim;
