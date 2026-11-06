use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::Command;
use tw_core::{Error, Result};

use crate::powershell::PowerShell;
use crate::{Checkpoint, Created, HostKey, Hypervisor, Preflight, StopMode, VmHandle, VmSpec, VmState, scripts};

/// Hyper-V through the PowerShell Hyper-V module.
#[derive(Debug, Clone)]
pub struct HyperV {
    ps: PowerShell,
    service_guid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupHostReport {
    pub added_to_group: bool,
    pub registered: bool,
    pub restart_required: bool,
}

impl HyperV {
    pub fn new(service_guid: impl Into<String>) -> Self {
        Self { ps: PowerShell::default(), service_guid: service_guid.into() }
    }

    async fn run(&self, body: &str, params: Value) -> Result<Value> {
        self.ps.run(body, &params).await
    }

    async fn run_ok(&self, body: &str, params: Value) -> Result<()> {
        self.run(body, params).await.map(drop)
    }

    /// The elevated, one-time host setup behind `terminal-windows setup-host`.
    pub async fn setup_host(&self, enable_hyper_v: bool) -> Result<SetupHostReport> {
        // Enabling a Windows feature can take minutes.
        let ps = self.ps.clone().with_timeout(Duration::from_secs(900));
        let result = ps
            .run(&scripts::setup_host(), &json!({ "enableHyperV": enable_hyper_v, "serviceGuid": self.service_guid }))
            .await?;
        Ok(SetupHostReport {
            added_to_group: result["addedToGroup"].as_bool().unwrap_or(false),
            registered: result["registered"].as_bool().unwrap_or(false),
            restart_required: result["restartRequired"].as_bool().unwrap_or(false),
        })
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// PowerShell collapses a one-element array into a bare object.
fn as_list(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

#[async_trait::async_trait]
impl Hypervisor for HyperV {
    fn id(&self) -> &'static str {
        "hyperv"
    }

    async fn preflight(&self) -> Result<Preflight> {
        let value = self.run(&scripts::preflight(), json!({ "serviceGuid": self.service_guid })).await?;
        Ok(serde_json::from_value(value)?)
    }

    async fn create(&self, spec: &VmSpec) -> Result<Created> {
        let params = json!({
            "hvName": spec.hv_name,
            "cpu": spec.cpu,
            "memoryBytes": u64::from(spec.memory_gib) << 30,
            "diskBytes": u64::from(spec.disk_gib) << 30,
            "width": spec.width,
            "height": spec.height,
            "network": spec.network,
            "vmPath": path_text(&spec.directory.join("hv")),
            "vhdPath": path_text(&spec.directory.join("disk.vhdx")),
        });
        let result = self.run(&scripts::create(), params).await?;
        Ok(Created {
            handle: VmHandle::new(&spec.hv_name),
            vm_id: result["id"].as_str().unwrap_or_default().to_string(),
            warning: result["warning"].as_str().map(str::to_string),
        })
    }

    async fn attach_dvd(&self, vm: &VmHandle, iso: &Path) -> Result<()> {
        self.run_ok(&scripts::attach_dvd(), json!({ "hvName": vm.hv_name, "path": path_text(iso) })).await
    }

    async fn set_boot_dvd(&self, vm: &VmHandle, iso: &Path) -> Result<()> {
        self.run_ok(&scripts::set_boot_dvd(), json!({ "hvName": vm.hv_name, "path": path_text(iso) })).await
    }

    async fn detach_dvds(&self, vm: &VmHandle) -> Result<()> {
        self.run_ok(&scripts::detach_dvds(), json!({ "hvName": vm.hv_name })).await
    }

    async fn start(&self, vm: &VmHandle) -> Result<()> {
        self.run_ok(&scripts::start(), json!({ "hvName": vm.hv_name })).await
    }

    async fn stop(&self, vm: &VmHandle, mode: StopMode) -> Result<()> {
        let params = json!({ "hvName": vm.hv_name });
        match mode {
            StopMode::Force => self.run_ok(&scripts::stop_force(), params).await,
            StopMode::Graceful => {
                let ps = self.ps.clone().with_timeout(Duration::from_secs(75));
                ps.run(&scripts::stop_graceful(), &params).await.map(drop).map_err(|e| {
                    Error::Other(format!("{e}. The guest may still be shutting down; use stop --force if needed"))
                })
            }
        }
    }

    async fn pause(&self, vm: &VmHandle) -> Result<()> {
        self.run_ok(&scripts::pause(), json!({ "hvName": vm.hv_name })).await
    }

    async fn resume(&self, vm: &VmHandle) -> Result<()> {
        self.run_ok(&scripts::resume(), json!({ "hvName": vm.hv_name })).await
    }

    async fn state(&self, vm: &VmHandle) -> Result<VmState> {
        let value = self.run(&scripts::state(), json!({ "hvName": vm.hv_name })).await?;
        Ok(VmState::parse(value["state"].as_str().unwrap_or_default()))
    }

    async fn vm_id(&self, vm: &VmHandle) -> Result<String> {
        let value = self.run(&scripts::state(), json!({ "hvName": vm.hv_name })).await?;
        value["id"].as_str().map(str::to_string).ok_or_else(|| Error::Other("Hyper-V returned no VM id".into()))
    }

    async fn press_key(&self, vm: &VmHandle, key: HostKey) -> Result<()> {
        let params = match key {
            HostKey::CtrlAltDel => json!({ "hvName": vm.hv_name, "kind": "ctrl-alt-del" }),
            HostKey::Space => json!({ "hvName": vm.hv_name, "kind": "key", "code": 0x20 }),
            HostKey::Enter => json!({ "hvName": vm.hv_name, "kind": "key", "code": 0x0D }),
        };
        self.run_ok(&scripts::keyboard(), params).await
    }

    async fn type_text(&self, vm: &VmHandle, text: &str) -> Result<()> {
        if !text.is_ascii() {
            return Err(Error::Usage("The hypervisor keyboard can only type ASCII text".into()));
        }
        self.run_ok(&scripts::keyboard(), json!({ "hvName": vm.hv_name, "kind": "text", "text": text })).await
    }

    async fn checkpoint_create(&self, vm: &VmHandle, label: &str) -> Result<()> {
        self.run_ok(&scripts::checkpoint_create(), json!({ "hvName": vm.hv_name, "label": label })).await
    }

    async fn checkpoint_list(&self, vm: &VmHandle) -> Result<Vec<Checkpoint>> {
        let value = self.run(&scripts::checkpoint_list(), json!({ "hvName": vm.hv_name })).await?;
        as_list(value).into_iter().map(|item| Ok(serde_json::from_value(item)?)).collect()
    }

    async fn checkpoint_restore(&self, vm: &VmHandle, label: &str) -> Result<()> {
        self.run_ok(&scripts::checkpoint_restore(), json!({ "hvName": vm.hv_name, "label": label })).await
    }

    async fn checkpoint_remove(&self, vm: &VmHandle, label: &str) -> Result<()> {
        self.run_ok(&scripts::checkpoint_remove(), json!({ "hvName": vm.hv_name, "label": label })).await
    }

    async fn open_console(&self, vm: &VmHandle) -> Result<()> {
        Command::new("vmconnect.exe").args(["localhost", &vm.hv_name]).spawn().map(drop).map_err(|e| {
            Error::Prerequisite(format!("Cannot start vmconnect.exe (install the Hyper-V GUI management tools): {e}"))
        })
    }

    async fn close_console(&self, vm: &VmHandle) -> Result<()> {
        self.run_ok(&scripts::close_console(), json!({ "hvName": vm.hv_name })).await
    }

    async fn remove(&self, vm: &VmHandle) -> Result<()> {
        self.run_ok(&scripts::remove(), json!({ "hvName": vm.hv_name })).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_item_results_become_lists() {
        assert!(as_list(Value::Null).is_empty());
        assert_eq!(as_list(json!({"a": 1})).len(), 1);
        assert_eq!(as_list(json!([1, 2])).len(), 2);
    }

    #[tokio::test]
    async fn the_keyboard_rejects_non_ascii_before_running_anything() {
        let hv = HyperV::new("00000000-0000-0000-0000-000000000000");
        let error = hv.type_text(&VmHandle::new("tw-x-00000000"), "caf\u{e9}").await.unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }

    /// Runs the real preflight script. It works on any Windows host, including Home.
    #[cfg(windows)]
    #[tokio::test]
    async fn preflight_runs_on_this_host() {
        let hv = HyperV::new("b7a4d3c0-5e21-4f8a-9c6d-2e81f0a7d315");
        let preflight = hv.preflight().await.unwrap();
        assert!(!preflight.caption.is_empty());
        assert!(preflight.logical_processors >= 1);
        assert!(preflight.ram_bytes > 0);
    }
}
