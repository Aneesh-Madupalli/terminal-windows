use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tw_core::{Error, Result};

use crate::{Checkpoint, Created, HostKey, Hypervisor, Preflight, StopMode, VmHandle, VmSpec, VmState};

#[derive(Debug, Clone)]
struct FakeCheckpoint {
    name: String,
    state: VmState,
}

#[derive(Debug, Clone)]
struct FakeVm {
    id: String,
    state: VmState,
    dvds: Vec<PathBuf>,
    boot_dvd: Option<PathBuf>,
    checkpoints: Vec<FakeCheckpoint>,
    keys: Vec<HostKey>,
    typed: String,
    console_open: bool,
    display: Option<(u32, u32)>,
}

#[derive(Debug, Default)]
struct Inner {
    vms: BTreeMap<String, FakeVm>,
    calls: Vec<String>,
}

/// An in-memory hypervisor that follows Hyper-V's state rules, for tests of the orchestration code.
#[derive(Debug)]
pub struct FakeHypervisor {
    inner: Mutex<Inner>,
    tcp_port: Mutex<Option<u16>>,
    id: &'static str,
    preflight: Preflight,
}

impl Default for FakeHypervisor {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            tcp_port: Mutex::new(None),
            id: "fake",
            preflight: Preflight {
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
            },
        }
    }
}

impl FakeHypervisor {
    pub fn with_preflight(preflight: Preflight) -> Self {
        Self { inner: Mutex::default(), tcp_port: Mutex::new(None), id: "fake", preflight }
    }

    /// Every operation performed so far, as `name arg`.
    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// Reports `id` as the backend, so tests can run several fakes side by side.
    pub fn with_id(mut self, id: &'static str) -> Self {
        self.id = id;
        self
    }

    /// Makes `endpoint` answer with a forwarded TCP port, as the VirtualBox backend does.
    pub fn use_tcp_port(&self, port: u16) {
        *self.tcp_port.lock().unwrap() = Some(port);
    }

    pub fn keys(&self, vm: &VmHandle) -> Vec<HostKey> {
        self.inner.lock().unwrap().vms.get(&vm.hv_name).map(|v| v.keys.clone()).unwrap_or_default()
    }

    pub fn typed(&self, vm: &VmHandle) -> String {
        self.inner.lock().unwrap().vms.get(&vm.hv_name).map(|v| v.typed.clone()).unwrap_or_default()
    }

    pub fn dvds(&self, vm: &VmHandle) -> Vec<PathBuf> {
        self.inner.lock().unwrap().vms.get(&vm.hv_name).map(|v| v.dvds.clone()).unwrap_or_default()
    }

    pub fn boot_dvd(&self, vm: &VmHandle) -> Option<PathBuf> {
        self.inner.lock().unwrap().vms.get(&vm.hv_name).and_then(|v| v.boot_dvd.clone())
    }

    pub fn display(&self, vm: &VmHandle) -> Option<(u32, u32)> {
        self.inner.lock().unwrap().vms.get(&vm.hv_name).and_then(|v| v.display)
    }

    pub fn console_open(&self, vm: &VmHandle) -> bool {
        self.inner.lock().unwrap().vms.get(&vm.hv_name).is_some_and(|v| v.console_open)
    }

    /// Runs `action` on the named VM, recording the call.
    fn with_vm<T>(&self, call: &str, vm: &VmHandle, action: impl FnOnce(&mut FakeVm) -> Result<T>) -> Result<T> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(format!("{call} {}", vm.hv_name));
        let entry = inner.vms.get_mut(&vm.hv_name).ok_or_else(|| {
            Error::Other(format!("Hyper-V was unable to find a virtual machine with name '{}'", vm.hv_name))
        })?;
        action(entry)
    }
}

fn wrong_state(action: &str, state: &VmState) -> Error {
    Error::Other(format!("Cannot {action} a VM that is {}", state.as_str()))
}

#[async_trait::async_trait]
impl Hypervisor for FakeHypervisor {
    fn id(&self) -> &'static str {
        self.id
    }

    async fn preflight(&self) -> Result<Preflight> {
        self.inner.lock().unwrap().calls.push("preflight".into());
        Ok(self.preflight.clone())
    }

    async fn create(&self, spec: &VmSpec) -> Result<Created> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(format!("create {}", spec.hv_name));
        if inner.vms.contains_key(&spec.hv_name) {
            return Err(Error::Other(format!("A VM named {} already exists", spec.hv_name)));
        }
        let id = format!("00000000-0000-0000-0000-{:012}", inner.vms.len() + 1);
        inner.vms.insert(
            spec.hv_name.clone(),
            FakeVm {
                id: id.clone(),
                state: VmState::Off,
                dvds: Vec::new(),
                boot_dvd: None,
                checkpoints: Vec::new(),
                keys: Vec::new(),
                typed: String::new(),
                console_open: false,
                display: None,
            },
        );
        Ok(Created { handle: VmHandle::new(&spec.hv_name), vm_id: id, warning: None })
    }

    async fn attach_dvd(&self, vm: &VmHandle, iso: &Path) -> Result<()> {
        self.with_vm("attach_dvd", vm, |v| {
            v.dvds.push(iso.to_path_buf());
            Ok(())
        })
    }

    async fn set_boot_dvd(&self, vm: &VmHandle, iso: &Path) -> Result<()> {
        self.with_vm("set_boot_dvd", vm, |v| {
            if !v.dvds.iter().any(|d| d == iso) {
                return Err(Error::Other("The install media is not attached".into()));
            }
            v.boot_dvd = Some(iso.to_path_buf());
            Ok(())
        })
    }

    async fn detach_dvds(&self, vm: &VmHandle) -> Result<()> {
        self.with_vm("detach_dvds", vm, |v| {
            v.dvds.clear();
            v.boot_dvd = None;
            Ok(())
        })
    }

    async fn start(&self, vm: &VmHandle) -> Result<()> {
        self.with_vm("start", vm, |v| match v.state {
            VmState::Off | VmState::Saved => {
                v.state = VmState::Running;
                Ok(())
            }
            VmState::Running => Ok(()),
            ref other => Err(wrong_state("start", other)),
        })
    }

    async fn stop(&self, vm: &VmHandle, mode: StopMode) -> Result<()> {
        self.with_vm("stop", vm, |v| match (&v.state, mode) {
            (VmState::Running, _) | (VmState::Paused, StopMode::Force) => {
                v.state = VmState::Off;
                Ok(())
            }
            (VmState::Off, _) => Ok(()),
            (other, _) => Err(wrong_state("stop", other)),
        })
    }

    async fn pause(&self, vm: &VmHandle) -> Result<()> {
        self.with_vm("pause", vm, |v| match v.state {
            VmState::Running => {
                v.state = VmState::Paused;
                Ok(())
            }
            ref other => Err(wrong_state("pause", other)),
        })
    }

    async fn resume(&self, vm: &VmHandle) -> Result<()> {
        self.with_vm("resume", vm, |v| match v.state {
            VmState::Paused => {
                v.state = VmState::Running;
                Ok(())
            }
            ref other => Err(wrong_state("resume", other)),
        })
    }

    async fn state(&self, vm: &VmHandle) -> Result<VmState> {
        self.with_vm("state", vm, |v| Ok(v.state.clone()))
    }

    async fn vm_id(&self, vm: &VmHandle) -> Result<String> {
        self.with_vm("vm_id", vm, |v| Ok(v.id.clone()))
    }

    async fn endpoint(&self, vm: &VmHandle) -> Result<crate::Endpoint> {
        if let Some(port) = *self.tcp_port.lock().unwrap() {
            return Ok(crate::Endpoint::Tcp { port });
        }
        Ok(crate::Endpoint::HvSock { vm_id: self.vm_id(vm).await? })
    }

    async fn press_key(&self, vm: &VmHandle, key: HostKey) -> Result<()> {
        self.with_vm("press_key", vm, |v| {
            if v.state != VmState::Running {
                return Err(wrong_state("send keys to", &v.state));
            }
            v.keys.push(key);
            Ok(())
        })
    }

    async fn type_text(&self, vm: &VmHandle, text: &str) -> Result<()> {
        if !text.is_ascii() {
            return Err(Error::Usage("The hypervisor keyboard can only type ASCII text".into()));
        }
        self.with_vm("type_text", vm, |v| {
            if v.state != VmState::Running {
                return Err(wrong_state("type into", &v.state));
            }
            v.typed.push_str(text);
            Ok(())
        })
    }

    async fn checkpoint_create(&self, vm: &VmHandle, label: &str) -> Result<()> {
        self.with_vm("checkpoint_create", vm, |v| {
            v.checkpoints.push(FakeCheckpoint { name: label.to_string(), state: v.state.clone() });
            Ok(())
        })
    }

    async fn checkpoint_list(&self, vm: &VmHandle) -> Result<Vec<Checkpoint>> {
        self.with_vm("checkpoint_list", vm, |v| {
            Ok(v.checkpoints
                .iter()
                .map(|c| Checkpoint { name: c.name.clone(), created: "2026-01-01T00:00:00.0000000Z".into() })
                .collect())
        })
    }

    async fn checkpoint_restore(&self, vm: &VmHandle, label: &str) -> Result<()> {
        self.with_vm("checkpoint_restore", vm, |v| {
            let checkpoint = v
                .checkpoints
                .iter()
                .find(|c| c.name == label)
                .ok_or_else(|| Error::Other(format!("No checkpoint named {label}")))?;
            // A Standard checkpoint also holds memory, so the VM returns to the state it had.
            v.state = checkpoint.state.clone();
            Ok(())
        })
    }

    async fn checkpoint_remove(&self, vm: &VmHandle, label: &str) -> Result<()> {
        self.with_vm("checkpoint_remove", vm, |v| {
            let before = v.checkpoints.len();
            v.checkpoints.retain(|c| c.name != label);
            if v.checkpoints.len() == before {
                return Err(Error::Other(format!("No checkpoint named {label}")));
            }
            Ok(())
        })
    }

    async fn open_console(&self, vm: &VmHandle) -> Result<()> {
        self.with_vm("open_console", vm, |v| {
            v.console_open = true;
            Ok(())
        })
    }

    async fn close_console(&self, vm: &VmHandle) -> Result<()> {
        self.with_vm("close_console", vm, |v| {
            v.console_open = false;
            Ok(())
        })
    }

    async fn set_display(&self, vm: &VmHandle, width: u32, height: u32) -> Result<()> {
        self.with_vm("set_display", vm, |v| {
            if v.state != VmState::Off {
                return Err(Error::Other("The VM must be stopped to change its display size".into()));
            }
            v.display = Some((width, height));
            Ok(())
        })
    }

    async fn remove(&self, vm: &VmHandle) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(format!("remove {}", vm.hv_name));
        inner.vms.remove(&vm.hv_name).map(drop).ok_or_else(|| Error::Other("No such VM".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> VmSpec {
        VmSpec {
            hv_name: name.into(),
            cpu: 4,
            memory_gib: 8,
            disk_gib: 64,
            width: 1280,
            height: 800,
            network: true,
            directory: PathBuf::from("C:\\vm"),
        }
    }

    async fn created(hv: &FakeHypervisor, name: &str) -> VmHandle {
        hv.create(&spec(name)).await.unwrap().handle
    }

    #[tokio::test]
    async fn lifecycle_follows_hyper_v_rules() {
        let hv = FakeHypervisor::default();
        let vm = created(&hv, "tw-a-00000000").await;
        assert_eq!(hv.state(&vm).await.unwrap(), VmState::Off);
        assert!(hv.pause(&vm).await.is_err(), "cannot pause a stopped VM");
        hv.start(&vm).await.unwrap();
        hv.start(&vm).await.unwrap();
        hv.pause(&vm).await.unwrap();
        assert!(hv.stop(&vm, StopMode::Graceful).await.is_err(), "a paused VM cannot shut down gracefully");
        hv.resume(&vm).await.unwrap();
        hv.stop(&vm, StopMode::Graceful).await.unwrap();
        assert_eq!(hv.state(&vm).await.unwrap(), VmState::Off);
        hv.start(&vm).await.unwrap();
        hv.pause(&vm).await.unwrap();
        hv.stop(&vm, StopMode::Force).await.unwrap();
    }

    #[tokio::test]
    async fn names_are_unique_and_ids_stable() {
        let hv = FakeHypervisor::default();
        let first = hv.create(&spec("tw-a-00000000")).await.unwrap();
        assert!(hv.create(&spec("tw-a-00000000")).await.is_err());
        let second = hv.create(&spec("tw-b-00000000")).await.unwrap();
        assert_ne!(first.vm_id, second.vm_id);
        assert_eq!(hv.vm_id(&first.handle).await.unwrap(), first.vm_id);
    }

    #[tokio::test]
    async fn install_media_flow() {
        let hv = FakeHypervisor::default();
        let vm = created(&hv, "tw-a-00000000").await;
        let windows = Path::new("C:\\win.iso");
        let answer = Path::new("C:\\answer.iso");
        assert!(hv.set_boot_dvd(&vm, windows).await.is_err(), "boot media must be attached first");
        hv.attach_dvd(&vm, windows).await.unwrap();
        hv.attach_dvd(&vm, answer).await.unwrap();
        hv.set_boot_dvd(&vm, windows).await.unwrap();
        assert_eq!(hv.boot_dvd(&vm).as_deref(), Some(windows));
        hv.detach_dvds(&vm).await.unwrap();
        assert!(hv.dvds(&vm).is_empty());
        assert_eq!(hv.boot_dvd(&vm), None);
    }

    #[tokio::test]
    async fn keys_need_a_running_vm() {
        let hv = FakeHypervisor::default();
        let vm = created(&hv, "tw-a-00000000").await;
        assert!(hv.press_key(&vm, HostKey::Space).await.is_err());
        hv.start(&vm).await.unwrap();
        hv.press_key(&vm, HostKey::Space).await.unwrap();
        hv.press_key(&vm, HostKey::CtrlAltDel).await.unwrap();
        hv.type_text(&vm, "hunter2").await.unwrap();
        assert_eq!(hv.keys(&vm), [HostKey::Space, HostKey::CtrlAltDel]);
        assert_eq!(hv.typed(&vm), "hunter2");
        assert!(hv.type_text(&vm, "caf\u{e9}").await.is_err());
    }

    #[tokio::test]
    async fn restoring_a_checkpoint_restores_the_running_state() {
        let hv = FakeHypervisor::default();
        let vm = created(&hv, "tw-a-00000000").await;
        hv.start(&vm).await.unwrap();
        hv.checkpoint_create(&vm, "clean").await.unwrap();
        hv.stop(&vm, StopMode::Force).await.unwrap();
        hv.checkpoint_restore(&vm, "clean").await.unwrap();
        assert_eq!(hv.state(&vm).await.unwrap(), VmState::Running);
        assert_eq!(hv.checkpoint_list(&vm).await.unwrap().len(), 1);
        assert!(hv.checkpoint_restore(&vm, "missing").await.is_err());
        hv.checkpoint_remove(&vm, "clean").await.unwrap();
        assert!(hv.checkpoint_remove(&vm, "clean").await.is_err());
    }

    #[tokio::test]
    async fn unknown_vms_error_and_calls_are_recorded() {
        let hv = FakeHypervisor::default();
        let ghost = VmHandle::new("tw-ghost-00000000");
        assert!(hv.start(&ghost).await.is_err());
        let vm = created(&hv, "tw-a-00000000").await;
        hv.open_console(&vm).await.unwrap();
        assert!(hv.console_open(&vm));
        hv.close_console(&vm).await.unwrap();
        hv.remove(&vm).await.unwrap();
        assert!(hv.state(&vm).await.is_err());
        let calls = hv.calls();
        assert!(calls.contains(&"create tw-a-00000000".to_string()));
        assert!(calls.contains(&"remove tw-a-00000000".to_string()));
    }

    #[tokio::test]
    async fn usable_as_a_trait_object() {
        let hv: Box<dyn Hypervisor> = Box::new(FakeHypervisor::default());
        hv.preflight().await.unwrap().check_ready().unwrap();
        assert_eq!(hv.id(), "fake");
    }
}
