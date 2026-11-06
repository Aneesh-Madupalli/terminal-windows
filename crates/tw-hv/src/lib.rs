//! The hypervisor abstraction, the Hyper-V backend (PowerShell), and an in-memory fake for tests.

pub mod fake;
pub mod hyperv;
pub mod powershell;
pub mod scripts;
pub mod virtualbox;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tw_core::{Error, Result};

pub use fake::FakeHypervisor;
pub use hyperv::HyperV;
pub use powershell::PowerShell;
pub use virtualbox::VirtualBox;

/// Identifies a VM by its Hyper-V name (`tw-<name>-<usertag>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmHandle {
    pub hv_name: String,
}

impl VmHandle {
    pub fn new(hv_name: impl Into<String>) -> Self {
        Self { hv_name: hv_name.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSpec {
    pub hv_name: String,
    pub cpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub width: u32,
    pub height: u32,
    pub network: bool,
    /// `machines\<name>`: the VHDX and the Hyper-V configuration live below it.
    pub directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub handle: VmHandle,
    pub vm_id: String,
    /// Set when a best-effort step (such as the display size) was not accepted.
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum VmState {
    Off,
    Starting,
    Running,
    Paused,
    Saved,
    Stopping,
    Other(String),
}

impl VmState {
    pub fn parse(text: &str) -> Self {
        match text {
            "Off" => Self::Off,
            "Starting" => Self::Starting,
            "Running" | "RunningCritical" => Self::Running,
            "Paused" | "PausedCritical" => Self::Paused,
            "Saved" | "SavedCritical" | "FastSaved" => Self::Saved,
            "Stopping" | "ShuttingDown" | "Saving" | "FastSaving" => Self::Stopping,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Off => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Saved => "saved",
            Self::Stopping => "stopping",
            Self::Other(other) => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    Graceful,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKey {
    Space,
    Enter,
    CtrlAltDel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub name: String,
    pub created: String,
}

/// How the host reaches the guest agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A Hyper-V socket addressed by the VM's GUID.
    HvSock { vm_id: String },
    /// A forwarded TCP port on 127.0.0.1.
    Tcp { port: u16 },
}

fn default_backend() -> String {
    "hyperv".into()
}

/// What the host can do before any VM exists (`doctor` and `setup-host` read this).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    pub windows: String,
    pub caption: String,
    pub hypervisor_present: bool,
    pub vmms_status: String,
    pub hyper_v_module: bool,
    pub elevated: bool,
    pub in_hyper_v_administrators: bool,
    pub service_registered: bool,
    pub vmconnect: bool,
    pub logical_processors: u32,
    pub ram_bytes: u64,
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Set by backends other than Hyper-V when the host cannot create VMs, with the next step.
    #[serde(default)]
    pub problem: Option<String>,
}

impl Preflight {
    /// The first reason this host cannot create VMs, with the next step for the user.
    pub fn check_ready(&self) -> Result<()> {
        if let Some(problem) = &self.problem {
            return Err(Error::Prerequisite(problem.clone()));
        }
        if self.backend != "hyperv" {
            return Ok(());
        }
        if self.caption.contains("Home") {
            return Err(Error::Prerequisite(
                "Windows Home does not include Hyper-V. Use Windows Pro, Enterprise or Education".into(),
            ));
        }
        if self.vmms_status == "missing" {
            return Err(Error::Prerequisite(
                "Hyper-V is not enabled. From an elevated prompt run: terminal-windows setup-host --enable-hyper-v, then restart"
                    .into(),
            ));
        }
        if !self.hyper_v_module {
            return Err(Error::Prerequisite(
                "The Hyper-V PowerShell module is missing. Run: terminal-windows setup-host --enable-hyper-v".into(),
            ));
        }
        if !self.elevated && !self.in_hyper_v_administrators {
            return Err(Error::AccessDenied(
                "Your account cannot manage Hyper-V. From an elevated prompt run: terminal-windows setup-host, then sign out and in"
                    .into(),
            ));
        }
        if !self.service_registered {
            return Err(Error::Prerequisite(
                "The guest control channel is not registered. From an elevated prompt run: terminal-windows setup-host"
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn host_info(&self) -> tw_core::HostInfo {
        tw_core::HostInfo { logical_cpus: self.logical_processors, ram_gib: (self.ram_bytes >> 30) as u32 }
    }
}

#[async_trait::async_trait]
pub trait Hypervisor: Send + Sync {
    fn id(&self) -> &'static str;

    async fn preflight(&self) -> Result<Preflight>;

    /// Creates the VM, its disk and devices. The VM is not started.
    async fn create(&self, spec: &VmSpec) -> Result<Created>;
    async fn attach_dvd(&self, vm: &VmHandle, iso: &Path) -> Result<()>;
    /// Makes the drive holding `iso` the first boot device.
    async fn set_boot_dvd(&self, vm: &VmHandle, iso: &Path) -> Result<()>;
    /// Removes every DVD drive and boots from the system disk.
    async fn detach_dvds(&self, vm: &VmHandle) -> Result<()>;

    async fn start(&self, vm: &VmHandle) -> Result<()>;
    async fn stop(&self, vm: &VmHandle, mode: StopMode) -> Result<()>;
    async fn pause(&self, vm: &VmHandle) -> Result<()>;
    async fn resume(&self, vm: &VmHandle) -> Result<()>;
    async fn state(&self, vm: &VmHandle) -> Result<VmState>;
    /// The VM's GUID, which the host socket connects to. Re-read on demand because it changes on import.
    async fn vm_id(&self, vm: &VmHandle) -> Result<String>;
    /// Where the guest agent listens. Hyper-V sockets unless the backend says otherwise.
    async fn endpoint(&self, vm: &VmHandle) -> Result<Endpoint> {
        Ok(Endpoint::HvSock { vm_id: self.vm_id(vm).await? })
    }

    async fn press_key(&self, vm: &VmHandle, key: HostKey) -> Result<()>;
    async fn type_text(&self, vm: &VmHandle, text: &str) -> Result<()>;

    async fn checkpoint_create(&self, vm: &VmHandle, label: &str) -> Result<()>;
    async fn checkpoint_list(&self, vm: &VmHandle) -> Result<Vec<Checkpoint>>;
    async fn checkpoint_restore(&self, vm: &VmHandle, label: &str) -> Result<()>;
    async fn checkpoint_remove(&self, vm: &VmHandle, label: &str) -> Result<()>;

    async fn open_console(&self, vm: &VmHandle) -> Result<()>;
    async fn close_console(&self, vm: &VmHandle) -> Result<()>;
    /// Deletes the VM registration. The caller deletes the files.
    async fn remove(&self, vm: &VmHandle) -> Result<()>;
    /// Changes the guest display size. The VM must be off.
    async fn set_display(&self, _vm: &VmHandle, _width: u32, _height: u32) -> Result<()> {
        Err(Error::Prerequisite("This backend cannot change the display size".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Preflight {
        Preflight {
            windows: "10.0.26100".into(),
            caption: "Microsoft Windows 11 Pro".into(),
            hypervisor_present: true,
            vmms_status: "Running".into(),
            hyper_v_module: true,
            elevated: false,
            in_hyper_v_administrators: true,
            service_registered: true,
            vmconnect: true,
            logical_processors: 16,
            ram_bytes: 32 << 30,
            backend: "hyperv".into(),
            problem: None,
        }
    }

    #[test]
    fn a_ready_host_passes() {
        ready().check_ready().unwrap();
        assert_eq!(ready().host_info().ram_gib, 32);
    }

    #[test]
    fn each_failure_has_its_own_error_and_exit_code() {
        let mut home = ready();
        home.caption = "Microsoft Windows 11 Home Single Language".into();
        assert_eq!(home.check_ready().unwrap_err().exit_code(), 3);

        let mut no_vmms = ready();
        no_vmms.vmms_status = "missing".into();
        assert_eq!(no_vmms.check_ready().unwrap_err().exit_code(), 3);

        let mut no_module = ready();
        no_module.hyper_v_module = false;
        assert_eq!(no_module.check_ready().unwrap_err().exit_code(), 3);

        let mut no_rights = ready();
        no_rights.in_hyper_v_administrators = false;
        assert_eq!(no_rights.check_ready().unwrap_err().exit_code(), 7);

        let mut elevated = no_rights.clone();
        elevated.elevated = true;
        elevated.check_ready().unwrap();

        let mut unregistered = ready();
        unregistered.service_registered = false;
        assert_eq!(unregistered.check_ready().unwrap_err().exit_code(), 3);
    }

    #[test]
    fn states_parse() {
        assert_eq!(VmState::parse("Off"), VmState::Off);
        assert_eq!(VmState::parse("Running"), VmState::Running);
        assert_eq!(VmState::parse("FastSaved"), VmState::Saved);
        assert_eq!(VmState::parse("Reset").as_str(), "Reset");
        assert_eq!(VmState::Off.as_str(), "stopped");
    }
}
