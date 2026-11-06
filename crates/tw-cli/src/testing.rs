//! Test doubles and a ready-made environment for the command tests.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tw_client::{ClientError, Guest, GuestError, Reply};
use tw_core::{Result, Store};
use tw_hv::{FakeHypervisor, Preflight};
use tw_provision::iso::IsoInfo;
use tw_provision::wim::{Architecture, ImageInfo};

use crate::env::{AnswerRequest, Credentials, Env, Media, Timing};

pub fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

pub fn ready_preflight() -> Preflight {
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

pub fn home_preflight() -> Preflight {
    Preflight { caption: "Microsoft Windows 11 Home".into(), vmms_status: "missing".into(), ..ready_preflight() }
}

fn image(index: u32, name: &str, architecture: Architecture) -> ImageInfo {
    ImageInfo {
        index,
        name: name.into(),
        description: name.into(),
        display_name: name.into(),
        edition_id: String::new(),
        architecture,
        major: 10,
        minor: 0,
        build: 26100,
    }
}

#[derive(Default)]
pub struct FakeMedia {
    requests: Mutex<Vec<(String, u32)>>,
    channels: Mutex<Vec<(String, bool)>>,
}

impl FakeMedia {
    /// `(guest channel, hardware-check bypass)` for each answer ISO built.
    pub fn answer_channels(&self) -> Vec<(String, bool)> {
        self.channels.lock().unwrap().clone()
    }

    /// `(image name, image index)` for each answer ISO built.
    pub fn answer_requests(&self) -> Vec<(String, u32)> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Media for FakeMedia {
    async fn inspect(&self, iso: &Path) -> Result<IsoInfo> {
        Ok(IsoInfo {
            path: iso.to_path_buf(),
            image_file: "install.wim".into(),
            images: vec![image(1, "Windows 11 Home", Architecture::X64), image(2, "Windows 11 Pro", Architecture::X64)],
        })
    }

    async fn build_answer(&self, request: &AnswerRequest, output: &Path) -> Result<()> {
        std::fs::write(output, b"answer iso")?;
        let name = if request.options.image_index == 2 { "Windows 11 Pro" } else { "Windows 11 Home" };
        self.requests.lock().unwrap().push((name.into(), request.options.image_index));
        self.channels
            .lock()
            .unwrap()
            .push((request.config.options.channel.clone(), request.options.bypass_hardware_checks));
        Ok(())
    }
}

#[derive(Default)]
pub struct FakeCredentials {
    items: Mutex<BTreeMap<String, (String, String)>>,
}

impl FakeCredentials {
    pub fn has(&self, target: &str) -> bool {
        self.items.lock().unwrap().contains_key(target)
    }

    pub fn count(&self) -> usize {
        self.items.lock().unwrap().len()
    }
}

impl Credentials for FakeCredentials {
    fn store(&self, target: &str, user: &str, password: &str) -> Result<()> {
        self.items.lock().unwrap().insert(target.into(), (user.into(), password.into()));
        Ok(())
    }

    fn fetch(&self, target: &str) -> Result<Option<(String, String)>> {
        Ok(self.items.lock().unwrap().get(target).cloned())
    }

    fn remove(&self, target: &str) -> Result<()> {
        self.items.lock().unwrap().remove(target);
        Ok(())
    }
}

type Upload = (String, BTreeMap<String, String>, usize);

/// An agent that is unreachable for a number of status calls and then ready.
pub struct FakeGuest {
    unreachable: Mutex<u32>,
    pub build: Mutex<u32>,
    pub session: Mutex<String>,
    pub calls: Mutex<Vec<(String, BTreeMap<String, String>)>>,
    pub uploads: Mutex<Vec<Upload>>,
    pub fail_with: Mutex<Option<String>>,
}

impl FakeGuest {
    pub fn new() -> Self {
        Self {
            unreachable: Mutex::new(0),
            build: Mutex::new(1),
            session: Mutex::new("ready".into()),
            calls: Mutex::new(Vec::new()),
            uploads: Mutex::new(Vec::new()),
            fail_with: Mutex::new(None),
        }
    }

    pub fn script_unreachable(&self, count: u32) {
        *self.unreachable.lock().unwrap() = count;
    }
}

fn reply(pairs: &[(&str, String)], payload: Vec<u8>) -> Reply {
    Reply { values: pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(), payload }
}

#[async_trait::async_trait]
impl Guest for FakeGuest {
    async fn call(
        &self,
        action: &str,
        args: BTreeMap<String, String>,
        _: Duration,
    ) -> std::result::Result<Reply, GuestError> {
        if action == "status" {
            let mut unreachable = self.unreachable.lock().unwrap();
            if *unreachable > 0 {
                *unreachable -= 1;
                return Err(GuestError::Client(ClientError::Connect(std::io::ErrorKind::ConnectionRefused.into())));
            }
            return Ok(reply(
                &[
                    ("session", self.session.lock().unwrap().clone()),
                    ("version", "0.1.0".into()),
                    ("build", self.build.lock().unwrap().to_string()),
                    ("os", "Windows 11 Pro 24H2 (build 26100)".into()),
                    ("width", "1280".into()),
                    ("height", "800".into()),
                ],
                Vec::new(),
            ));
        }
        self.calls.lock().unwrap().push((action.to_string(), args));
        if let Some(message) = self.fail_with.lock().unwrap().clone() {
            return Err(GuestError::Client(ClientError::Remote { code: Some("bad-request".into()), message }));
        }
        match action {
            "screenshot" => Ok(reply(&[("width", "1280".into())], b"\x89PNG-fake".to_vec())),
            "logs" => Ok(reply(&[("lines", "2".into())], b"line one\nline two".to_vec())),
            _ => Ok(reply(&[("ok", "true".into())], Vec::new())),
        }
    }

    async fn ctrl_alt_del(&self) -> std::result::Result<(), GuestError> {
        Ok(())
    }

    async fn upload(
        &self,
        action: &str,
        args: BTreeMap<String, String>,
        payload: Vec<u8>,
        _: Duration,
    ) -> std::result::Result<Reply, GuestError> {
        self.uploads.lock().unwrap().push((action.to_string(), args, payload.len()));
        Ok(reply(&[("staged", "true".into()), ("restarting", "true".into())], Vec::new()))
    }
}

pub struct Harness {
    pub env: Env,
    pub hv: Arc<FakeHypervisor>,
    pub hypervisors: Vec<Arc<FakeHypervisor>>,
    pub media: Arc<FakeMedia>,
    pub credentials: Arc<FakeCredentials>,
    pub guest: Arc<FakeGuest>,
    pub dir: tempfile::TempDir,
    progress: Arc<Mutex<Vec<String>>>,
}

impl Harness {
    pub fn new() -> Self {
        Self::with_hypervisor(FakeHypervisor::with_preflight(ready_preflight()))
    }

    pub fn with_hypervisor(hv: FakeHypervisor) -> Self {
        Self::with_backends(vec![hv])
    }

    /// Several backends in order of preference; `hv` is the first.
    pub fn with_backends(backends: Vec<FakeHypervisor>) -> Self {
        let hypervisors: Vec<Arc<FakeHypervisor>> = backends.into_iter().map(Arc::new).collect();
        let hv = hypervisors[0].clone();
        let dir = tempfile::tempdir().unwrap();
        let media = Arc::new(FakeMedia::default());
        let credentials = Arc::new(FakeCredentials::default());
        let guest = Arc::new(FakeGuest::new());
        let progress = Arc::new(Mutex::new(Vec::new()));
        let sink = progress.clone();
        let shared = guest.clone();
        let agent_exe = dir.path().join("twagent.exe");
        std::fs::write(&agent_exe, vec![b'M'; 2048]).unwrap();
        let env = Env {
            store: Store::at(dir.path().join("data")).unwrap(),
            hypervisors: hypervisors.iter().map(|h| h.clone() as Arc<dyn tw_hv::Hypervisor>).collect(),
            media: media.clone(),
            guest: Arc::new(move |_| shared.clone()),
            credentials: credentials.clone(),
            agent_exe,
            user_tag: "0a1b2c3d".into(),
            timing: Timing {
                poll: Duration::from_millis(2),
                boot_keys: Duration::from_millis(20),
                install: Duration::from_millis(250),
                ready: Duration::from_millis(250),
            },
            progress: Arc::new(move |m| sink.lock().unwrap().push(m.to_string())),
        };
        Self { env, hv, hypervisors, media, credentials, guest, dir, progress }
    }

    pub fn progress(&self) -> Vec<String> {
        self.progress.lock().unwrap().clone()
    }
}
