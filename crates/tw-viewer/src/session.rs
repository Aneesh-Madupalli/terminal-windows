//! What the viewer needs from the guest: a frame stream, requests, and the host-side extras.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tw_client::{FrameStream, Guest, GuestError, LiveGuest};
use tw_hv::{Hypervisor, VmHandle};

#[async_trait::async_trait]
pub trait Session: Send + Sync {
    async fn open_stream(&self) -> Result<FrameStream, GuestError>;
    async fn call(&self, action: &str, args: BTreeMap<String, String>) -> Result<(), GuestError>;
    async fn ctrl_alt_del(&self) -> Result<(), GuestError>;
    async fn open_console(&self) -> Result<(), GuestError>;
    /// A sentence for the status bar when something fails.
    fn explain(&self, error: &GuestError) -> String;
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct LiveSession {
    guest: LiveGuest,
    hypervisor: Arc<dyn Hypervisor>,
    vm: VmHandle,
    name: String,
}

impl LiveSession {
    pub fn new(guest: LiveGuest, hypervisor: Arc<dyn Hypervisor>, vm: VmHandle, name: impl Into<String>) -> Self {
        Self { guest, hypervisor, vm, name: name.into() }
    }
}

#[async_trait::async_trait]
impl Session for LiveSession {
    async fn open_stream(&self) -> Result<FrameStream, GuestError> {
        self.guest.open_stream().await
    }

    async fn call(&self, action: &str, args: BTreeMap<String, String>) -> Result<(), GuestError> {
        self.guest.call(action, args, REQUEST_TIMEOUT).await.map(drop)
    }

    async fn ctrl_alt_del(&self) -> Result<(), GuestError> {
        self.guest.ctrl_alt_del().await
    }

    async fn open_console(&self) -> Result<(), GuestError> {
        self.hypervisor.open_console(&self.vm).await.map_err(|e| GuestError::Other(e.to_string()))
    }

    fn explain(&self, error: &GuestError) -> String {
        error.explain(&self.name)
    }
}
