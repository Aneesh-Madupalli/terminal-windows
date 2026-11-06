use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::args::Arguments;
use crate::error::{Error, Result};
use crate::name::validate_name;

pub const SCHEMA: u32 = 1;

/// Value options shared by `up` and `create`.
pub const VM_OPTION_NAMES: &[&str] =
    &["cpus", "memory", "disk", "width", "height", "iso", "edition", "image-index", "locale", "uac", "backend"];
/// Flags shared by `up` and `create`.
pub const VM_FLAG_NAMES: &[&str] = &["offline"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostInfo {
    pub logical_cpus: u32,
    pub ram_gib: u32,
}

/// Resources for a new VM, validated against PRD FR-VM-2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmOptions {
    pub cpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub width: u32,
    pub height: u32,
    pub network: bool,
}

impl Default for VmOptions {
    fn default() -> Self {
        Self { cpu: 4, memory_gib: 8, disk_gib: 64, width: 1280, height: 800, network: true }
    }
}

impl VmOptions {
    pub fn from_args(args: &Arguments, host: HostInfo) -> Result<Self> {
        if host.logical_cpus < 2 {
            return Err(Error::Prerequisite("This host needs at least 2 logical processors".into()));
        }
        let default_cpu = (host.logical_cpus / 2).clamp(2, 4);
        let cpu = args.integer("cpus", i64::from(default_cpu), 2..=i64::from(host.logical_cpus))?;

        // The default never takes more than half of the host's RAM.
        let default_memory = (host.ram_gib / 2).min(8);
        if args.option("memory").is_none() && default_memory < 4 {
            return Err(Error::Prerequisite(
                "This host has too little memory for the default VM (needs at least 8 GiB of RAM)".into(),
            ));
        }
        let memory = args.integer("memory", i64::from(default_memory), 4..=i64::from(host.ram_gib.min(128)))?;

        let disk = args.integer("disk", 64, 64..=2048)?;
        let width = args.integer("width", 1280, 800..=1920)?;
        let height = args.integer("height", 800, 600..=1200)?;
        Ok(Self {
            cpu: cpu as u32,
            memory_gib: memory as u32,
            disk_gib: disk as u32,
            width: width as u32,
            height: height as u32,
            network: !args.flag("offline"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub version: String,
    pub build: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub schema: u32,
    pub name: String,
    pub hv_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_id: Option<String>,
    pub backend: String,
    pub cpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub width: u32,
    pub height: u32,
    pub network: bool,
    pub installed: bool,
    pub guest_os: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentInfo>,
    pub created_at: String,
    pub credential_target: String,
    /// Fields written by newer versions are kept when the record is rewritten.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Machine {
    pub fn new(name: &str, hv_name: &str, options: VmOptions) -> Self {
        Self {
            schema: SCHEMA,
            name: name.to_string(),
            hv_name: hv_name.to_string(),
            vm_id: None,
            backend: "hyperv".into(),
            cpu: options.cpu,
            memory_gib: options.memory_gib,
            disk_gib: options.disk_gib,
            width: options.width,
            height: options.height,
            network: options.network,
            installed: false,
            guest_os: "windows".into(),
            guest_version: None,
            agent: None,
            created_at: utc_now_rfc3339(),
            credential_target: format!("terminal-windows/{name}/agent"),
            extra: BTreeMap::new(),
        }
    }
}

/// The data root: `machines/<name>/machine.json` and friends.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn at(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(Error::Usage("The data directory must be an absolute path".into()));
        }
        Ok(Self { root })
    }

    /// `TERMINAL_WINDOWS_HOME`, else `%LOCALAPPDATA%\terminal-windows`.
    pub fn from_env() -> Result<Self> {
        let root = std::env::var_os("TERMINAL_WINDOWS_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("terminal-windows")))
            .ok_or_else(|| Error::Prerequisite("Cannot find the data directory. Set TERMINAL_WINDOWS_HOME".into()))?;
        Self::at(root)
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn directory(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        Ok(self.root.join("machines").join(name))
    }

    pub fn load(&self, name: &str) -> Result<Machine> {
        let path = self.directory(name)?.join("machine.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NoSuchVm(name.to_string())),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes to a temporary file in the same directory, then renames over the record.
    pub fn save(&self, machine: &Machine) -> Result<()> {
        let dir = self.directory(&machine.name)?;
        std::fs::create_dir_all(&dir)?;
        let temporary = dir.join(format!("machine.json.tmp-{}", std::process::id()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(machine)?)?;
        std::fs::rename(&temporary, dir.join("machine.json"))?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Machine>> {
        let dir = self.root.join("machines");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut machines: Vec<Machine> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| self.load(&name).ok())
            .collect();
        machines.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(machines)
    }
}

fn utc_now_rfc3339() -> String {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = ((seconds / 86_400) as i64, seconds % 86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(input: &[&str]) -> Arguments {
        Arguments::parse(input, VM_FLAG_NAMES, VM_OPTION_NAMES).unwrap()
    }

    const HOST: HostInfo = HostInfo { logical_cpus: 16, ram_gib: 32 };

    #[test]
    fn defaults_follow_fr_vm_2() {
        let o = VmOptions::from_args(&args(&[]), HOST).unwrap();
        assert_eq!(o, VmOptions { cpu: 4, memory_gib: 8, disk_gib: 64, width: 1280, height: 800, network: true });
    }

    #[test]
    fn defaults_scale_down_on_small_hosts() {
        let o = VmOptions::from_args(&args(&[]), HostInfo { logical_cpus: 4, ram_gib: 16 }).unwrap();
        assert_eq!((o.cpu, o.memory_gib), (2, 8));
        let o = VmOptions::from_args(&args(&[]), HostInfo { logical_cpus: 8, ram_gib: 8 }).unwrap();
        assert_eq!(o.memory_gib, 4);
        assert!(VmOptions::from_args(&args(&[]), HostInfo { logical_cpus: 8, ram_gib: 6 }).is_err());
        assert!(VmOptions::from_args(&args(&[]), HostInfo { logical_cpus: 1, ram_gib: 32 }).is_err());
    }

    #[test]
    fn ranges_are_enforced() {
        for bad in [
            ["--cpus", "1"],
            ["--cpus", "17"],
            ["--memory", "3"],
            ["--memory", "33"],
            ["--disk", "63"],
            ["--disk", "2049"],
            ["--width", "799"],
            ["--width", "1921"],
            ["--height", "599"],
            ["--height", "1201"],
        ] {
            assert!(VmOptions::from_args(&args(&bad), HOST).is_err(), "{bad:?}");
        }
        let o = VmOptions::from_args(&args(&["--cpus", "2", "--memory", "4", "--offline"]), HOST).unwrap();
        assert!(!o.network);
    }

    #[test]
    fn store_round_trips_and_preserves_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path()).unwrap();
        let mut machine = Machine::new("work", "tw-work-3fa9c1d2", VmOptions::default());
        machine.installed = true;
        store.save(&machine).unwrap();

        // A newer version added a field; rewriting must keep it.
        let path = store.directory("work").unwrap().join("machine.json");
        let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["futureField"] = Value::from(42);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let loaded = store.load("work").unwrap();
        assert_eq!(loaded.extra.get("futureField"), Some(&Value::from(42)));
        store.save(&loaded).unwrap();
        assert_eq!(store.load("work").unwrap(), loaded);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn missing_vm_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path()).unwrap();
        assert!(matches!(store.load("ghost"), Err(Error::NoSuchVm(_))));
        assert!(store.load("bad name").is_err());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn relative_roots_are_rejected() {
        assert!(Store::at("relative/path").is_err());
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        let t = utc_now_rfc3339();
        assert_eq!(t.len(), 20);
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T');
    }
}
