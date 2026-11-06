//! VirtualBox backend through `VBoxManage`, for hosts without Hyper-V (Windows Home). The guest agent is
//! reached through a NAT port forward on 127.0.0.1 instead of a Hyper-V socket.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::process::Command;
use tw_core::{Error, Result};

use crate::powershell::PowerShell;
use crate::{
    Checkpoint, Created, Endpoint, HostKey, Hypervisor, Preflight, StopMode, VmHandle, VmSpec, VmState, scripts,
};

/// Name of the NAT rule that carries the agent connection.
const RULE: &str = "tw";
const CONTROLLER: &str = "SATA";
const AGENT_PORT: u16 = tw_proto::AGENT_TCP_PORT;
const MIN_MAJOR: u32 = 6;
const GRACEFUL_WAIT: Duration = Duration::from_secs(90);

#[async_trait::async_trait]
pub trait Runner: Send + Sync {
    /// Runs `VBoxManage` with `args` and returns its standard output.
    async fn run(&self, args: &[String]) -> Result<String>;
}

pub struct ProcessRunner {
    exe: PathBuf,
}

#[async_trait::async_trait]
impl Runner for ProcessRunner {
    async fn run(&self, args: &[String]) -> Result<String> {
        let mut command = Command::new(&self.exe);
        command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let child = command.spawn().map_err(|e| {
            Error::Prerequisite(format!(
                "Cannot start VBoxManage ({}): {e}. Install VirtualBox 7 from virtualbox.org",
                self.exe.display()
            ))
        })?;
        let output = tokio::time::timeout(Duration::from_secs(600), child.wait_with_output())
            .await
            .map_err(|_| Error::Other("VBoxManage did not finish within 10 minutes".into()))??;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail =
            stderr.lines().rev().find(|l| l.contains("error") || l.contains("VBOX_E")).unwrap_or_else(|| stderr.trim());
        Err(Error::Other(format!("VBoxManage {} failed: {}", args.first().map_or("", String::as_str), detail.trim())))
    }
}

/// `VBoxManage.exe`: `VBOXMANAGE`, then the default install folder, then `PATH`.
pub fn locate() -> PathBuf {
    if let Some(path) = std::env::var_os("VBOXMANAGE") {
        return PathBuf::from(path);
    }
    for variable in ["ProgramFiles", "ProgramW6432"] {
        if let Some(base) = std::env::var_os(variable) {
            let candidate = Path::new(&base).join("Oracle").join("VirtualBox").join("VBoxManage.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("VBoxManage.exe")
}

pub struct VirtualBox {
    runner: Arc<dyn Runner>,
    ps: PowerShell,
    gui: Option<PathBuf>,
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// `KEY="value"` lines of `--machinereadable` output.
pub fn machine_readable(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.trim().trim_matches('"').to_string(), value.trim().trim_matches('"').to_string()))
        })
        .collect()
}

pub fn parse_state(text: &str) -> VmState {
    match text {
        "poweroff" | "aborted" | "aborted-saved" => VmState::Off,
        "running" => VmState::Running,
        "paused" | "teleporting-paused-vm" => VmState::Paused,
        "saved" => VmState::Saved,
        "starting" | "restoring" | "teleporting" | "live-snapshotting" => VmState::Starting,
        "stopping" | "saving" | "powering-off" => VmState::Stopping,
        other => VmState::Other(other.to_string()),
    }
}

/// The host port of the agent rule in a `Forwarding(n)="name,tcp,127.0.0.1,HOST,,GUEST"` entry.
pub fn forwarded_port(info: &BTreeMap<String, String>) -> Option<u16> {
    info.iter().filter(|(key, _)| key.starts_with("Forwarding(")).find_map(|(_, value)| {
        let parts: Vec<&str> = value.split(',').collect();
        (parts.first() == Some(&RULE) && parts.len() >= 4).then(|| parts[3].parse().ok()).flatten()
    })
}

/// DVD drives holding media: `(port, path)` on the SATA controller, excluding the system disk on port 0.
fn dvds(info: &BTreeMap<String, String>) -> Vec<(u32, String)> {
    let mut found: Vec<(u32, String)> = info
        .iter()
        .filter_map(|(key, value)| {
            let rest = key.strip_prefix(CONTROLLER)?.strip_prefix('-')?;
            let (port, device) = rest.split_once('-')?;
            let (port, device): (u32, u32) = (port.parse().ok()?, device.parse().ok()?);
            (device == 0 && port >= 1 && value != "none" && value != "emptydrive").then(|| (port, value.clone()))
        })
        .collect();
    found.sort();
    found
}

fn same_path(a: &str, b: &str) -> bool {
    a.replace('/', "\\").eq_ignore_ascii_case(&b.replace('/', "\\"))
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

impl VirtualBox {
    pub fn new() -> Self {
        let exe = locate();
        let gui = exe.parent().map(|dir| dir.join("VirtualBoxVM.exe")).filter(|path| path.is_file());
        Self::with_runner(Arc::new(ProcessRunner { exe }), gui)
    }

    pub fn with_runner(runner: Arc<dyn Runner>, gui: Option<PathBuf>) -> Self {
        Self { runner, ps: PowerShell::default(), gui }
    }

    async fn vbox(&self, args: &[&str]) -> Result<String> {
        self.runner.run(&strings(args)).await
    }

    async fn info(&self, vm: &VmHandle) -> Result<BTreeMap<String, String>> {
        Ok(machine_readable(&self.vbox(&["showvminfo", &vm.hv_name, "--machinereadable"]).await?))
    }

    async fn is_off(&self, vm: &VmHandle) -> Result<bool> {
        Ok(self.state(vm).await? == VmState::Off)
    }

    async fn scancodes(&self, vm: &VmHandle, codes: &[&str]) -> Result<()> {
        let mut args = vec!["controlvm", vm.hv_name.as_str(), "keyboardputscancode"];
        args.extend_from_slice(codes);
        self.vbox(&args).await.map(drop)
    }

    async fn existing_names(&self) -> Result<Vec<String>> {
        let listing = self.vbox(&["list", "vms"]).await?;
        Ok(listing.lines().filter_map(|l| Some(l.trim().strip_prefix('"')?.split('"').next()?.to_string())).collect())
    }

    async fn configure(&self, spec: &VmSpec, port: u16) -> Result<Option<String>> {
        let name = spec.hv_name.as_str();
        let memory = (u64::from(spec.memory_gib) * 1024).to_string();
        let cpus = spec.cpu.to_string();
        let rule = format!("{RULE},tcp,127.0.0.1,{port},,{AGENT_PORT}");
        self.vbox(&[
            "modifyvm",
            name,
            "--memory",
            &memory,
            "--cpus",
            &cpus,
            "--firmware",
            "efi",
            "--graphicscontroller",
            "vboxsvga",
            "--vram",
            "128",
            "--boot1",
            "dvd",
            "--boot2",
            "disk",
            "--boot3",
            "none",
            "--boot4",
            "none",
            "--ioapic",
            "on",
            "--clipboard-mode",
            "disabled",
            "--draganddrop",
            "disabled",
            "--nic1",
            "nat",
            "--nat-pf1",
            &rule,
        ])
        .await?;
        // A virtual TPM exists from VirtualBox 7; the answer file bypasses the check where it does not.
        let _ = self.vbox(&["modifyvm", name, "--tpm-type", "2.0"]).await;
        self.vbox(&[
            "storagectl",
            name,
            "--name",
            CONTROLLER,
            "--add",
            "sata",
            "--controller",
            "IntelAhci",
            "--portcount",
            "4",
        ])
        .await?;
        let disk = spec.directory.join("disk.vdi");
        let disk = disk.to_string_lossy();
        let size = (u64::from(spec.disk_gib) * 1024).to_string();
        self.vbox(&["createmedium", "disk", "--filename", &disk, "--size", &size, "--format", "VDI"]).await?;
        self.vbox(&[
            "storageattach",
            name,
            "--storagectl",
            CONTROLLER,
            "--port",
            "0",
            "--device",
            "0",
            "--type",
            "hdd",
            "--medium",
            &disk,
        ])
        .await?;
        let resolution = format!("{}x{}", spec.width, spec.height);
        let warning = self
            .vbox(&["setextradata", name, "VBoxInternal2/EfiGraphicsResolution", &resolution])
            .await
            .err()
            .map(|e| format!("Could not set the guest display size to {resolution}: {e}"));
        Ok(warning)
    }
}

impl Default for VirtualBox {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Hypervisor for VirtualBox {
    fn id(&self) -> &'static str {
        "virtualbox"
    }

    async fn preflight(&self) -> Result<Preflight> {
        let host = self.ps.run(&scripts::host_info(), &json!({})).await?;
        let problem = match self.vbox(&["--version"]).await {
            Err(_) => Some(
                "VirtualBox is not installed. Install VirtualBox 7 from https://www.virtualbox.org, then run: terminal-windows doctor"
                    .to_string(),
            ),
            Ok(version) => {
                let major: u32 = version.trim().split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
                (major < MIN_MAJOR).then(|| format!("VirtualBox {} is too old. Install VirtualBox 7 or newer", version.trim()))
            }
        };
        Ok(Preflight {
            windows: host["windows"].as_str().unwrap_or_default().to_string(),
            caption: host["caption"].as_str().unwrap_or_default().to_string(),
            hypervisor_present: host["hypervisorPresent"].as_bool().unwrap_or(false),
            vmms_status: "n/a".into(),
            hyper_v_module: false,
            elevated: host["elevated"].as_bool().unwrap_or(false),
            in_hyper_v_administrators: false,
            service_registered: true,
            vmconnect: false,
            logical_processors: host["logicalProcessors"].as_u64().unwrap_or(0) as u32,
            ram_bytes: host["ramBytes"].as_u64().unwrap_or(0),
            backend: "virtualbox".into(),
            problem,
        })
    }

    async fn create(&self, spec: &VmSpec) -> Result<Created> {
        let name = spec.hv_name.as_str();
        if self.existing_names().await?.iter().any(|n| n == name) {
            return Err(Error::Other(format!("A VM named {name} already exists")));
        }
        std::fs::create_dir_all(&spec.directory)?;
        let base = spec.directory.join("vbox");
        let base = base.to_string_lossy();
        let created = self
            .vbox(&["createvm", "--name", name, "--ostype", "Windows11_64", "--basefolder", &base, "--register"])
            .await;
        // Older VirtualBox releases do not know the Windows 11 type.
        if created.is_err() {
            self.vbox(&["createvm", "--name", name, "--ostype", "Windows10_64", "--basefolder", &base, "--register"])
                .await?;
        }
        let handle = VmHandle::new(name);
        let configured = self.configure(spec, free_port()?).await;
        let warning = match configured {
            Ok(warning) => warning,
            Err(error) => {
                let _ = self.vbox(&["unregistervm", name, "--delete"]).await;
                return Err(error);
            }
        };
        let vm_id = self.vm_id(&handle).await?;
        Ok(Created { handle, vm_id, warning })
    }

    async fn attach_dvd(&self, vm: &VmHandle, iso: &Path) -> Result<()> {
        let info = self.info(vm).await?;
        let used: Vec<u32> = dvds(&info).into_iter().map(|(port, _)| port).collect();
        let port = (1..=3u32)
            .find(|port| !used.contains(port))
            .ok_or_else(|| Error::Other("The VM has no free DVD drive".into()))?
            .to_string();
        let iso = iso.to_string_lossy();
        self.vbox(&[
            "storageattach",
            &vm.hv_name,
            "--storagectl",
            CONTROLLER,
            "--port",
            &port,
            "--device",
            "0",
            "--type",
            "dvddrive",
            "--medium",
            &iso,
        ])
        .await
        .map(drop)
    }

    async fn set_boot_dvd(&self, vm: &VmHandle, iso: &Path) -> Result<()> {
        let info = self.info(vm).await?;
        let drives = dvds(&info);
        let iso_text = iso.to_string_lossy();
        // The firmware boots the lowest-numbered drive, so the boot media must be attached first.
        match drives.first() {
            Some((_, path)) if same_path(path, &iso_text) => {}
            Some(_) if drives.iter().any(|(_, path)| same_path(path, &iso_text)) => {
                return Err(Error::Other("The install media must be attached before the answer media".into()));
            }
            _ => return Err(Error::Other("The install media is not attached".into())),
        }
        if self.is_off(vm).await? {
            self.vbox(&["modifyvm", &vm.hv_name, "--boot1", "dvd", "--boot2", "disk"]).await?;
        }
        Ok(())
    }

    async fn detach_dvds(&self, vm: &VmHandle) -> Result<()> {
        let info = self.info(vm).await?;
        for (port, _) in dvds(&info) {
            let port = port.to_string();
            self.vbox(&[
                "storageattach",
                &vm.hv_name,
                "--storagectl",
                CONTROLLER,
                "--port",
                &port,
                "--device",
                "0",
                "--type",
                "dvddrive",
                "--medium",
                "none",
                "--forceunmount",
            ])
            .await?;
        }
        // Boot order can only change while the VM is off; empty drives fall through to the disk anyway.
        if self.is_off(vm).await? {
            self.vbox(&["modifyvm", &vm.hv_name, "--boot1", "disk", "--boot2", "none"]).await?;
        }
        Ok(())
    }

    async fn start(&self, vm: &VmHandle) -> Result<()> {
        self.vbox(&["startvm", &vm.hv_name, "--type", "headless"]).await.map(drop)
    }

    async fn stop(&self, vm: &VmHandle, mode: StopMode) -> Result<()> {
        match mode {
            StopMode::Force => self.vbox(&["controlvm", &vm.hv_name, "poweroff"]).await.map(drop),
            StopMode::Graceful => {
                self.vbox(&["controlvm", &vm.hv_name, "acpipowerbutton"]).await?;
                let deadline = tokio::time::Instant::now() + GRACEFUL_WAIT;
                while tokio::time::Instant::now() < deadline {
                    if self.is_off(vm).await? {
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(Error::Other("The guest did not shut down in time. Use stop --force if needed".into()))
            }
        }
    }

    async fn pause(&self, vm: &VmHandle) -> Result<()> {
        self.vbox(&["controlvm", &vm.hv_name, "pause"]).await.map(drop)
    }

    async fn resume(&self, vm: &VmHandle) -> Result<()> {
        self.vbox(&["controlvm", &vm.hv_name, "resume"]).await.map(drop)
    }

    async fn state(&self, vm: &VmHandle) -> Result<VmState> {
        Ok(parse_state(self.info(vm).await?.get("VMState").map_or("", String::as_str)))
    }

    async fn vm_id(&self, vm: &VmHandle) -> Result<String> {
        self.info(vm).await?.remove("UUID").ok_or_else(|| Error::Other("VirtualBox returned no VM id".into()))
    }

    async fn endpoint(&self, vm: &VmHandle) -> Result<Endpoint> {
        let port = forwarded_port(&self.info(vm).await?)
            .ok_or_else(|| Error::Other("The VM has no agent port forward. Delete it and run up again".into()))?;
        Ok(Endpoint::Tcp { port })
    }

    async fn press_key(&self, vm: &VmHandle, key: HostKey) -> Result<()> {
        match key {
            HostKey::Space => self.scancodes(vm, &["39", "b9"]).await,
            HostKey::Enter => self.scancodes(vm, &["1c", "9c"]).await,
            HostKey::CtrlAltDel => self.scancodes(vm, &["1d", "38", "e0", "53", "e0", "d3", "b8", "9d"]).await,
        }
    }

    async fn type_text(&self, vm: &VmHandle, text: &str) -> Result<()> {
        if !text.is_ascii() {
            return Err(Error::Usage("The hypervisor keyboard can only type ASCII text".into()));
        }
        self.vbox(&["controlvm", &vm.hv_name, "keyboardputstring", text]).await.map(drop)
    }

    async fn checkpoint_create(&self, vm: &VmHandle, label: &str) -> Result<()> {
        self.vbox(&["snapshot", &vm.hv_name, "take", label]).await.map(drop)
    }

    async fn checkpoint_list(&self, vm: &VmHandle) -> Result<Vec<Checkpoint>> {
        match self.vbox(&["snapshot", &vm.hv_name, "list", "--machinereadable"]).await {
            Ok(text) => Ok(machine_readable(&text)
                .into_iter()
                .filter(|(key, _)| key == "SnapshotName" || key.starts_with("SnapshotName-"))
                .map(|(_, name)| Checkpoint { name, created: String::new() })
                .collect()),
            Err(error) if error.to_string().contains("does not have any snapshots") => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    async fn checkpoint_restore(&self, vm: &VmHandle, label: &str) -> Result<()> {
        // VirtualBox restores only a powered-off machine.
        if !self.is_off(vm).await? {
            self.vbox(&["controlvm", &vm.hv_name, "poweroff"]).await?;
        }
        self.vbox(&["snapshot", &vm.hv_name, "restore", label]).await.map(drop)
    }

    async fn checkpoint_remove(&self, vm: &VmHandle, label: &str) -> Result<()> {
        self.vbox(&["snapshot", &vm.hv_name, "delete", label]).await.map(drop)
    }

    async fn open_console(&self, vm: &VmHandle) -> Result<()> {
        let Some(gui) = &self.gui else {
            return Err(Error::Prerequisite("VirtualBoxVM.exe was not found next to VBoxManage.exe".into()));
        };
        Command::new(gui)
            .args(["--startvm", &vm.hv_name])
            .spawn()
            .map(drop)
            .map_err(|e| Error::Other(format!("Cannot open the VirtualBox window: {e}")))
    }

    async fn close_console(&self, _vm: &VmHandle) -> Result<()> {
        Ok(())
    }

    async fn remove(&self, vm: &VmHandle) -> Result<()> {
        self.vbox(&["unregistervm", &vm.hv_name, "--delete"]).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Machine {
        uuid: String,
        state: String,
        storage: BTreeMap<u32, String>,
        snapshots: Vec<String>,
        forwarding: String,
        boot1: String,
    }

    #[derive(Default)]
    struct Simulator {
        vms: Mutex<BTreeMap<String, Machine>>,
        calls: Mutex<Vec<Vec<String>>>,
        keys: Mutex<Vec<String>>,
        reject_windows11: bool,
        missing: bool,
    }

    fn option<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
    }

    #[async_trait::async_trait]
    impl Runner for Simulator {
        async fn run(&self, args: &[String]) -> Result<String> {
            self.calls.lock().unwrap().push(args.to_vec());
            if self.missing {
                return Err(Error::Prerequisite("no VBoxManage".into()));
            }
            let mut vms = self.vms.lock().unwrap();
            let fail = |message: &str| Err(Error::Other(message.to_string()));
            let name = args.get(1).cloned().unwrap_or_default();
            match args[0].as_str() {
                "--version" => Ok("7.0.18r162988\n".into()),
                "list" => Ok(vms.keys().map(|n| format!("\"{n}\" {{{}}}\n", vms[n].uuid)).collect()),
                "createvm" => {
                    let name = option(args, "--name").unwrap().to_string();
                    if self.reject_windows11 && option(args, "--ostype") == Some("Windows11_64") {
                        return fail("Invalid OS type");
                    }
                    let uuid = format!("00000000-0000-0000-0000-{:012}", vms.len() + 1);
                    vms.insert(
                        name.clone(),
                        Machine { uuid: uuid.clone(), state: "poweroff".into(), ..Machine::default() },
                    );
                    Ok(format!("UUID: {uuid}\n"))
                }
                "createmedium" => Ok(String::new()),
                "unregistervm" => {
                    vms.remove(&name).map(|_| String::new()).ok_or_else(|| Error::Other("not found".into()))
                }
                _ => {
                    let Some(vm) = vms.get_mut(&name) else { return fail("Could not find a registered machine") };
                    match args[0].as_str() {
                        "modifyvm" => {
                            if let Some(rule) = option(args, "--nat-pf1") {
                                vm.forwarding = rule.to_string();
                            }
                            if let Some(boot) = option(args, "--boot1") {
                                if vm.state == "running" {
                                    return fail("The machine is already locked");
                                }
                                vm.boot1 = boot.to_string();
                            }
                            Ok(String::new())
                        }
                        "storageattach" => {
                            let port: u32 = option(args, "--port").unwrap().parse().unwrap();
                            let medium = option(args, "--medium").unwrap();
                            if medium == "none" {
                                vm.storage.remove(&port);
                            } else {
                                vm.storage.insert(port, medium.to_string());
                            }
                            Ok(String::new())
                        }
                        "showvminfo" => {
                            let mut out = format!("name=\"{name}\"\nUUID=\"{}\"\nVMState=\"{}\"\n", vm.uuid, vm.state);
                            if !vm.forwarding.is_empty() {
                                out += &format!("Forwarding(0)=\"{}\"\n", vm.forwarding);
                            }
                            for (port, medium) in &vm.storage {
                                out += &format!("\"SATA-{port}-0\"=\"{medium}\"\n");
                            }
                            out += "\"SATA-3-0\"=\"emptydrive\"\n";
                            Ok(out)
                        }
                        "startvm" => {
                            if vm.state == "running" {
                                return fail("already running");
                            }
                            vm.state = "running".into();
                            Ok(String::new())
                        }
                        "controlvm" => {
                            match args[2].as_str() {
                                "poweroff" | "acpipowerbutton" => vm.state = "poweroff".into(),
                                "pause" => vm.state = "paused".into(),
                                "resume" => vm.state = "running".into(),
                                "keyboardputscancode" => self.keys.lock().unwrap().push(args[3..].join(" ")),
                                "keyboardputstring" => self.keys.lock().unwrap().push(args[3].clone()),
                                _ => return fail("bad controlvm"),
                            }
                            Ok(String::new())
                        }
                        "snapshot" => match args[2].as_str() {
                            "take" => {
                                vm.snapshots.push(args[3].clone());
                                Ok(String::new())
                            }
                            "delete" => {
                                vm.snapshots.retain(|s| s != &args[3]);
                                Ok(String::new())
                            }
                            "restore" => {
                                if vm.state != "poweroff" {
                                    return fail("not powered off");
                                }
                                if vm.snapshots.contains(&args[3]) {
                                    Ok(String::new())
                                } else {
                                    fail("no such snapshot")
                                }
                            }
                            "list" if vm.snapshots.is_empty() => fail("This machine does not have any snapshots"),
                            "list" => Ok(vm
                                .snapshots
                                .iter()
                                .enumerate()
                                .map(|(i, s)| {
                                    if i == 0 {
                                        format!("SnapshotName=\"{s}\"\n")
                                    } else {
                                        format!("SnapshotName-{i}=\"{s}\"\n")
                                    }
                                })
                                .collect()),
                            _ => fail("bad snapshot"),
                        },
                        _ => Ok(String::new()),
                    }
                }
            }
        }
    }

    fn spec() -> VmSpec {
        VmSpec {
            hv_name: "tw-default-0a1b2c3d".into(),
            cpu: 4,
            memory_gib: 8,
            disk_gib: 64,
            width: 1280,
            height: 800,
            network: true,
            directory: tempfile::tempdir().unwrap().keep(),
        }
    }

    fn backend(simulator: Simulator) -> (VirtualBox, Arc<Simulator>) {
        let simulator = Arc::new(simulator);
        (VirtualBox::with_runner(simulator.clone(), None), simulator)
    }

    async fn created(vb: &VirtualBox) -> VmHandle {
        vb.create(&spec()).await.unwrap().handle
    }

    #[test]
    fn output_parsers() {
        assert_eq!(parse_state("poweroff"), VmState::Off);
        assert_eq!(parse_state("aborted"), VmState::Off);
        assert_eq!(parse_state("running"), VmState::Running);
        assert_eq!(parse_state("saved"), VmState::Saved);
        assert_eq!(parse_state("saving"), VmState::Stopping);
        assert_eq!(parse_state("gurumeditation").as_str(), "gurumeditation");
        let info = machine_readable(
            "name=\"a b\"\nForwarding(0)=\"tw,tcp,127.0.0.1,50123,,47831\"\nForwarding(1)=\"other,tcp,,1,,2\"\n\"SATA-1-0\"=\"C:\\\\x.iso\"\n",
        );
        assert_eq!(info["name"], "a b");
        assert_eq!(forwarded_port(&info), Some(50123));
        assert_eq!(dvds(&info), vec![(1, "C:\\\\x.iso".to_string())]);
        assert_eq!(forwarded_port(&machine_readable("x=1")), None);
        assert!(same_path("C:/A/b.iso", "c:\\a\\B.ISO"));
    }

    #[tokio::test]
    async fn create_configures_the_vm_and_reports_the_agent_port() {
        let (vb, sim) = backend(Simulator::default());
        let vm = created(&vb).await;
        assert_eq!(vb.state(&vm).await.unwrap(), VmState::Off);
        let Endpoint::Tcp { port } = vb.endpoint(&vm).await.unwrap() else { panic!("a TCP endpoint") };
        assert!(port > 1024);
        let calls = sim.calls.lock().unwrap().clone();
        let modify = calls.iter().find(|c| c[0] == "modifyvm" && c.contains(&"--firmware".to_string())).unwrap();
        assert_eq!(option(modify, "--firmware"), Some("efi"));
        assert_eq!(option(modify, "--memory"), Some("8192"));
        assert_eq!(option(modify, "--nat-pf1"), Some(format!("tw,tcp,127.0.0.1,{port},,47831").as_str()));
        assert!(calls.iter().any(|c| c[0] == "setextradata" && c.contains(&"1280x800".to_string())));
        assert!(calls.iter().any(|c| c[0] == "createmedium" && c.contains(&"65536".to_string())));
        assert!(vb.create(&spec()).await.is_err(), "the name is already taken");
    }

    #[tokio::test]
    async fn an_old_virtualbox_falls_back_to_the_windows_10_type() {
        let (vb, sim) = backend(Simulator { reject_windows11: true, ..Simulator::default() });
        created(&vb).await;
        let calls = sim.calls.lock().unwrap().clone();
        assert!(calls.iter().any(|c| c[0] == "createvm" && c.contains(&"Windows10_64".to_string())));
    }

    #[tokio::test]
    async fn install_media_boots_from_the_first_drive() {
        let (vb, _) = backend(Simulator::default());
        let vm = created(&vb).await;
        let (windows, answer) = (Path::new("C:\\win.iso"), Path::new("C:\\answer.iso"));
        assert!(vb.set_boot_dvd(&vm, windows).await.is_err(), "nothing is attached yet");
        vb.attach_dvd(&vm, windows).await.unwrap();
        vb.attach_dvd(&vm, answer).await.unwrap();
        vb.set_boot_dvd(&vm, windows).await.unwrap();
        assert!(vb.set_boot_dvd(&vm, answer).await.is_err(), "the answer media is not the first drive");
        vb.detach_dvds(&vm).await.unwrap();
        assert!(dvds(&vb.info(&vm).await.unwrap()).is_empty());
    }

    #[tokio::test]
    async fn detaching_while_running_does_not_touch_the_boot_order() {
        let (vb, _) = backend(Simulator::default());
        let vm = created(&vb).await;
        vb.attach_dvd(&vm, Path::new("C:\\win.iso")).await.unwrap();
        vb.start(&vm).await.unwrap();
        vb.detach_dvds(&vm).await.unwrap();
        assert_eq!(vb.state(&vm).await.unwrap(), VmState::Running);
    }

    #[tokio::test]
    async fn the_lifecycle_follows_virtualbox_states() {
        let (vb, _) = backend(Simulator::default());
        let vm = created(&vb).await;
        vb.start(&vm).await.unwrap();
        assert_eq!(vb.state(&vm).await.unwrap(), VmState::Running);
        assert!(vb.start(&vm).await.is_err());
        vb.pause(&vm).await.unwrap();
        assert_eq!(vb.state(&vm).await.unwrap(), VmState::Paused);
        vb.resume(&vm).await.unwrap();
        vb.stop(&vm, StopMode::Graceful).await.unwrap();
        assert_eq!(vb.state(&vm).await.unwrap(), VmState::Off);
        vb.start(&vm).await.unwrap();
        vb.stop(&vm, StopMode::Force).await.unwrap();
        vb.remove(&vm).await.unwrap();
        assert!(vb.state(&vm).await.is_err());
    }

    #[tokio::test]
    async fn host_keys_are_sent_as_scancodes() {
        let (vb, sim) = backend(Simulator::default());
        let vm = created(&vb).await;
        vb.press_key(&vm, HostKey::Space).await.unwrap();
        vb.press_key(&vm, HostKey::Enter).await.unwrap();
        vb.press_key(&vm, HostKey::CtrlAltDel).await.unwrap();
        vb.type_text(&vm, "hello").await.unwrap();
        assert_eq!(sim.keys.lock().unwrap().as_slice(), ["39 b9", "1c 9c", "1d 38 e0 53 e0 d3 b8 9d", "hello"]);
        assert_eq!(vb.type_text(&vm, "caf\u{e9}").await.unwrap_err().exit_code(), 2);
    }

    #[tokio::test]
    async fn checkpoints_round_trip_and_restore_powers_off_first() {
        let (vb, _) = backend(Simulator::default());
        let vm = created(&vb).await;
        assert!(vb.checkpoint_list(&vm).await.unwrap().is_empty());
        vb.checkpoint_create(&vm, "clean").await.unwrap();
        vb.checkpoint_create(&vm, "later").await.unwrap();
        let names: Vec<String> = vb.checkpoint_list(&vm).await.unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["clean", "later"]);
        vb.start(&vm).await.unwrap();
        vb.checkpoint_restore(&vm, "clean").await.unwrap();
        assert_eq!(vb.state(&vm).await.unwrap(), VmState::Off);
        vb.checkpoint_remove(&vm, "later").await.unwrap();
        assert_eq!(vb.checkpoint_list(&vm).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn preflight_reports_a_missing_virtualbox_with_the_next_step() {
        let (vb, _) = backend(Simulator { missing: true, ..Simulator::default() });
        let preflight = vb.preflight().await.unwrap();
        assert_eq!(preflight.backend, "virtualbox");
        assert!(!preflight.caption.is_empty(), "host facts still come from PowerShell");
        assert!(preflight.check_ready().unwrap_err().to_string().contains("virtualbox.org"));
    }

    #[tokio::test]
    async fn a_ready_virtualbox_host_passes_even_on_windows_home() {
        let (vb, _) = backend(Simulator::default());
        let preflight = vb.preflight().await.unwrap();
        assert_eq!(preflight.problem, None);
        preflight.check_ready().unwrap();
        assert!(preflight.host_info().logical_cpus >= 1);
    }
}
