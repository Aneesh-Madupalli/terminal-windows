//! Everything the commands need from the outside world, behind traits so the orchestration is testable.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tw_client::Guest;
use tw_core::{Error, Machine, Result, Store};
use tw_hv::{Hypervisor, PowerShell, Preflight};
use tw_provision::answer::AnswerOptions;
use tw_provision::config::GuestConfig;
use tw_provision::iso::{self, AnswerMedia, IsoInfo};

pub trait Credentials: Send + Sync {
    fn store(&self, target: &str, user: &str, password: &str) -> Result<()>;
    fn fetch(&self, target: &str) -> Result<Option<(String, String)>>;
    fn remove(&self, target: &str) -> Result<()>;
}

/// Windows Credential Manager.
pub struct SystemCredentials;

impl Credentials for SystemCredentials {
    fn store(&self, target: &str, user: &str, password: &str) -> Result<()> {
        tw_provision::credential::store(target, user, password)
    }

    fn fetch(&self, target: &str) -> Result<Option<(String, String)>> {
        tw_provision::credential::fetch(target)
    }

    fn remove(&self, target: &str) -> Result<()> {
        tw_provision::credential::remove(target)
    }
}

/// What goes on the answer ISO.
pub struct AnswerRequest {
    pub options: AnswerOptions,
    pub config: GuestConfig,
    pub agent_exe: PathBuf,
}

#[async_trait::async_trait]
pub trait Media: Send + Sync {
    async fn inspect(&self, iso: &Path) -> Result<IsoInfo>;
    async fn build_answer(&self, request: &AnswerRequest, output: &Path) -> Result<()>;
}

pub struct PsMedia {
    ps: PowerShell,
}

impl PsMedia {
    pub fn new() -> Self {
        Self { ps: PowerShell::default().with_timeout(Duration::from_secs(300)) }
    }
}

impl Default for PsMedia {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Media for PsMedia {
    async fn inspect(&self, iso: &Path) -> Result<IsoInfo> {
        iso::inspect(&self.ps, iso).await
    }

    async fn build_answer(&self, request: &AnswerRequest, output: &Path) -> Result<()> {
        let media = AnswerMedia { options: &request.options, config: &request.config, agent_exe: &request.agent_exe };
        iso::build_answer_iso(&self.ps, &media, output).await
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How often readiness is polled.
    pub poll: Duration,
    /// How long the boot key is pressed after the first start.
    pub boot_keys: Duration,
    /// How long Windows installation may take.
    pub install: Duration,
    /// How long an installed VM may take to become ready.
    pub ready: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(10),
            boot_keys: Duration::from_secs(30),
            install: Duration::from_secs(120 * 60),
            ready: Duration::from_secs(5 * 60),
        }
    }
}

pub type GuestFactory = Arc<dyn Fn(&str) -> Arc<dyn Guest> + Send + Sync>;

pub struct Env {
    pub store: Store,
    /// Backends in order of preference; a machine remembers the one it was created with.
    pub hypervisors: Vec<Arc<dyn Hypervisor>>,
    pub media: Arc<dyn Media>,
    pub guest: GuestFactory,
    pub credentials: Arc<dyn Credentials>,
    pub agent_exe: PathBuf,
    pub user_tag: String,
    pub timing: Timing,
    pub progress: Arc<dyn Fn(&str) + Send + Sync>,
}

impl Env {
    pub fn say(&self, message: &str) {
        (self.progress)(message);
    }

    pub fn backend(&self, id: &str) -> Result<Arc<dyn Hypervisor>> {
        self.hypervisors.iter().find(|hv| hv.id() == id).cloned().ok_or_else(|| {
            let known: Vec<&str> = self.hypervisors.iter().map(|hv| hv.id()).collect();
            Error::Usage(format!("Unknown backend '{id}'. Available: {}", known.join(", ")))
        })
    }

    /// The backend a machine was created with.
    pub fn hv_for(&self, machine: &Machine) -> Result<Arc<dyn Hypervisor>> {
        self.backend(&machine.backend)
    }

    /// The requested backend, or the first one that can create VMs on this host.
    pub async fn choose_backend(&self, requested: Option<&str>) -> Result<(Arc<dyn Hypervisor>, Preflight)> {
        if let Some(id) = requested {
            let hv = self.backend(id)?;
            let preflight = hv.preflight().await?;
            preflight.check_ready()?;
            return Ok((hv, preflight));
        }
        let mut problems = Vec::new();
        for hv in &self.hypervisors {
            let outcome = match hv.preflight().await {
                Ok(preflight) => preflight.check_ready().map(|()| preflight),
                Err(error) => Err(error),
            };
            match outcome {
                Ok(preflight) => return Ok((hv.clone(), preflight)),
                Err(error) => problems.push((hv.id(), error)),
            }
        }
        match problems.as_slice() {
            [(_, only)] => Err(Error::Prerequisite(only.to_string())),
            _ => Err(Error::Prerequisite(format!(
                "No virtualization backend is ready:\n{}\nRun: terminal-windows doctor",
                problems.iter().map(|(id, e)| format!("  {id}: {e}")).collect::<Vec<_>>().join("\n")
            ))),
        }
    }
}

/// Lets an `Arc<dyn Guest>` be used where a `Guest` value is expected.
pub struct SharedGuest(pub Arc<dyn Guest>);

#[async_trait::async_trait]
impl Guest for SharedGuest {
    async fn call(
        &self,
        action: &str,
        args: std::collections::BTreeMap<String, String>,
        timeout: Duration,
    ) -> std::result::Result<tw_client::Reply, tw_client::GuestError> {
        self.0.call(action, args, timeout).await
    }

    async fn ctrl_alt_del(&self) -> std::result::Result<(), tw_client::GuestError> {
        self.0.ctrl_alt_del().await
    }

    async fn upload(
        &self,
        action: &str,
        args: std::collections::BTreeMap<String, String>,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> std::result::Result<tw_client::Reply, tw_client::GuestError> {
        self.0.upload(action, args, payload, timeout).await
    }
}
