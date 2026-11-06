//! `up`: from a Windows ISO to a signed-in guest with a working agent, resumable at every step.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tw_client::{ClientError, GuestError};
use tw_core::args::Arguments;
use tw_core::machine::{AgentInfo, VM_FLAG_NAMES, VM_OPTION_NAMES};
use tw_core::{Error, GuestReadiness, Machine, Result, SetupPlan, VmOptions, hv_name, name::computer_name, secret};
use tw_hv::{HostKey, Hypervisor, VmHandle, VmSpec, VmState};
use tw_provision::answer::{AGENT_USER, AnswerOptions, generate_password, validate_locale};
use tw_provision::config::GuestConfig;
use tw_provision::wim;

use crate::env::{AnswerRequest, Env};

const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

pub fn plan_text(plan: &SetupPlan) -> String {
    let mut text = format!("Plan for '{}':\n", plan.name);
    for (index, step) in plan.steps.iter().enumerate() {
        text.push_str(&format!("  {}. {step}\n", index + 1));
    }
    text
}

fn load_optional(env: &Env, name: &str) -> Result<Option<Machine>> {
    match env.store.load(name) {
        Ok(machine) => Ok(Some(machine)),
        Err(Error::NoSuchVm(_)) => Ok(None),
        Err(other) => Err(other),
    }
}

/// Turns a guest failure into the error and exit code the CLI reports.
pub fn guest_error(name: &str, error: &GuestError) -> Error {
    let message = error.explain(name);
    match error {
        GuestError::NotCreated(_) => Error::NoSuchVm(name.to_string()),
        GuestError::NotRunning { .. } => Error::NotRunning(name.to_string()),
        GuestError::Client(ClientError::Connect(_) | ClientError::Timeout { .. }) => Error::GuestNotReady(message),
        GuestError::Client(ClientError::Remote { code: Some(code), .. }) if code == "locked" => Error::Locked(message),
        GuestError::Client(ClientError::Remote { code: Some(code), .. })
            if code == "secure-desktop" || code == "no-session" =>
        {
            Error::GuestNotReady(message)
        }
        _ => Error::Other(message),
    }
}

fn phase_of(readiness: GuestReadiness) -> &'static str {
    match readiness {
        GuestReadiness::Ready => "ready",
        GuestReadiness::Locked => "The guest is locked",
        GuestReadiness::NoSession => "Waiting for the guest to sign in",
        GuestReadiness::SecureDesktop => "A UAC prompt or sign-in screen is showing in the guest",
        GuestReadiness::CaptureError => "The guest screen cannot be captured yet",
        GuestReadiness::Unavailable => "Installing Windows and waiting for the guest agent",
    }
}

/// Polls the agent until the guest desktop is ready. Returns the agent's status values.
pub async fn wait_ready(env: &Env, machine: &Machine, timeout: Duration) -> Result<BTreeMap<String, String>> {
    let guest = (env.guest)(&machine.name);
    let hv = env.hv_for(machine)?;
    let handle = VmHandle::new(&machine.hv_name);
    let deadline = Instant::now() + timeout;
    let mut last = "";
    loop {
        if matches!(hv.state(&handle).await?, VmState::Off | VmState::Saved) {
            return Err(Error::NotRunning(machine.name.clone()));
        }
        let phase = match guest.call("status", BTreeMap::new(), STATUS_TIMEOUT).await {
            Ok(reply) => {
                let readiness = GuestReadiness::from_session(reply.values.get("session").map(String::as_str));
                if readiness == GuestReadiness::Ready {
                    return Ok(reply.values);
                }
                phase_of(readiness)
            }
            Err(GuestError::NotRunning { .. }) => "Waiting for the VM to start",
            Err(_) => phase_of(GuestReadiness::Unavailable),
        };
        if phase != last {
            env.say(phase);
            last = phase;
        }
        if Instant::now() >= deadline {
            return Err(Error::GuestNotReady(format!(
                "The guest is not ready after {} minutes ({}). Run `terminal-windows view {}` to see it, then `terminal-windows up {}` to continue",
                timeout.as_secs() / 60,
                phase.to_lowercase(),
                machine.name,
                machine.name
            )));
        }
        tokio::time::sleep(env.timing.poll).await;
    }
}

/// Starts or resumes the VM, whatever state it is in.
pub async fn ensure_running(env: &Env, machine: &Machine) -> Result<bool> {
    let hv = env.hv_for(machine)?;
    let handle = VmHandle::new(&machine.hv_name);
    match hv.state(&handle).await? {
        VmState::Running => Ok(false),
        VmState::Paused => {
            hv.resume(&handle).await?;
            Ok(false)
        }
        VmState::Off | VmState::Saved => {
            hv.start(&handle).await?;
            Ok(true)
        }
        other => Err(Error::Other(format!("VM '{}' is {}; try again in a moment", machine.name, other.as_str()))),
    }
}

const UP_FLAGS: [&str; 3] = ["plan", "no-view", "detach"];

fn parse_up(rest: &[String]) -> Result<Arguments> {
    let mut flags = VM_FLAG_NAMES.to_vec();
    flags.extend(UP_FLAGS);
    Arguments::parse(rest, &flags, VM_OPTION_NAMES)
}

/// The machine to open in the viewer after `up`, or `None` when the options ask for no viewer.
pub fn view_target(rest: &[String]) -> Option<String> {
    let args = parse_up(rest).ok()?;
    if UP_FLAGS.iter().any(|flag| args.flag(flag)) {
        return None;
    }
    args.machine_name().ok()
}

fn up_json(machine: &Machine, state: &str) -> Result<String> {
    let agent = machine.agent.as_ref().map(|a| serde_json::json!({ "version": a.version, "build": a.build }));
    let value = serde_json::json!({
        "name": machine.name,
        "backend": machine.backend,
        "state": state,
        "os": machine.guest_version,
        "agent": agent,
    });
    Ok(serde_json::to_string_pretty(&value)? + "\n")
}

pub async fn up(env: &Env, rest: &[String]) -> Result<String> {
    let args = parse_up(rest)?;
    let detach = args.flag("detach");
    let json = detach || args.flag("no-view");
    let name = args.machine_name()?;
    let existing = load_optional(env, &name)?;
    if args.flag("plan") {
        return Ok(plan_text(&SetupPlan::new(&name, existing.as_ref())));
    }
    let (machine, ready) = match existing {
        Some(machine) if machine.installed => {
            let started = ensure_running(env, &machine).await?;
            if started {
                env.say("Starting the VM");
            }
            if detach {
                return up_json(&machine, "starting");
            }
            let values = wait_ready(env, &machine, env.timing.ready).await?;
            (remember_agent(env, machine, &values)?, true)
        }
        other => install(env, &name, &args, other).await?,
    };
    if json {
        return up_json(&machine, if ready { "ready" } else { "installing" });
    }
    if !ready {
        return Ok(format!(
            "VM '{0}' is installing Windows in the background. Follow it with: terminal-windows status {0}\n",
            machine.name
        ));
    }
    Ok(format!(
        "VM '{0}' is ready ({1}). Next: terminal-windows view {0}, or connect an agent with terminal-windows mcp {0}\n",
        machine.name,
        machine.guest_version.as_deref().unwrap_or("Windows")
    ))
}

fn remember_agent(env: &Env, mut machine: Machine, values: &BTreeMap<String, String>) -> Result<Machine> {
    if let (Some(version), Some(build)) = (values.get("version"), values.get("build").and_then(|b| b.parse().ok())) {
        machine.agent = Some(AgentInfo { version: version.clone(), build });
    }
    if let Some(os) = values.get("os") {
        machine.guest_version = Some(os.clone());
    }
    env.store.save(&machine)?;
    Ok(machine)
}

/// Creates the VM record, the channel secret and the guest account password.
async fn create_machine(env: &Env, hv: &dyn Hypervisor, name: &str, options: VmOptions) -> Result<Machine> {
    let hv_name = hv_name(name, &env.user_tag)?;
    let directory = env.store.directory(name)?;
    let spec = VmSpec {
        hv_name: hv_name.clone(),
        cpu: options.cpu,
        memory_gib: options.memory_gib,
        disk_gib: options.disk_gib,
        width: options.width,
        height: options.height,
        network: options.network,
        directory,
    };
    env.say("Creating the virtual machine");
    let created = hv.create(&spec).await?;
    if let Some(warning) = &created.warning {
        env.say(&format!("Warning: {warning}"));
    }
    let mut machine = Machine::new(name, &hv_name, options);
    machine.backend = hv.id().to_string();
    machine.vm_id = Some(created.vm_id);
    env.store.save(&machine)?;
    Ok(machine)
}

fn load_or_create_secret(env: &Env, name: &str) -> Result<Vec<u8>> {
    let path = env.store.directory(name)?.join("channel.secret");
    if path.exists() {
        return secret::load(&path);
    }
    let key = secret::generate();
    secret::save(&path, &key)?;
    Ok(key.to_vec())
}

fn load_or_create_password(env: &Env, machine: &Machine) -> Result<String> {
    if let Some((_, password)) = env.credentials.fetch(&machine.credential_target)? {
        return Ok(password);
    }
    let password = generate_password();
    env.credentials.store(&machine.credential_target, AGENT_USER, &password)?;
    Ok(password)
}

/// Installs Windows and waits for the agent. The flag is false when `--detach` returned before the guest was ready.
async fn install(env: &Env, name: &str, args: &Arguments, existing: Option<Machine>) -> Result<(Machine, bool)> {
    let iso = args
        .option("iso")
        .map(PathBuf::from)
        .or_else(|| existing.as_ref().and_then(|m| m.extra.get("iso")).and_then(|v| v.as_str()).map(PathBuf::from))
        .ok_or_else(|| Error::Usage("Provide the Windows ISO: terminal-windows up --iso PATH".into()))?;
    let locale = args.option("locale").unwrap_or("en-US").to_string();
    validate_locale(&locale)?;
    let uac_off = match args.option("uac") {
        None | Some("on") => false,
        Some("off") => true,
        Some(_) => return Err(Error::Usage("--uac must be on or off".into())),
    };
    let saved_index =
        existing.as_ref().and_then(|m| m.extra.get("image_index")).and_then(|v| v.as_u64()).map(|v| v as u32);
    let image_index = match args.option("image-index") {
        Some(_) => Some(args.integer("image-index", 1, 1..=64)? as u32),
        None if args.option("edition").is_none() => saved_index,
        None => None,
    };

    let (hv, preflight) = match &existing {
        Some(machine) => {
            let hv = env.hv_for(machine)?;
            let preflight = hv.preflight().await?;
            preflight.check_ready()?;
            (hv, preflight)
        }
        None => env.choose_backend(args.option("backend")).await?,
    };
    let options = VmOptions::from_args(args, preflight.host_info())?;

    env.say("Checking the Windows ISO");
    let info = env.media.inspect(&iso).await?;
    let image = wim::select(&info.images, image_index, args.option("edition"))?.clone();
    if let Some(warning) = image.support_warning() {
        env.say(&format!("Warning: {warning}"));
    }
    env.say(&format!("Installing {} (image {})", image.name, image.index));

    let mut machine = match existing {
        Some(machine) => machine,
        None => create_machine(env, &*hv, name, options).await?,
    };
    machine.extra.insert("iso".into(), iso.to_string_lossy().into_owned().into());
    machine.extra.insert("image_index".into(), image.index.into());
    env.store.save(&machine)?;

    let handle = VmHandle::new(&machine.hv_name);
    let started = if matches!(hv.state(&handle).await?, VmState::Off | VmState::Saved) {
        let key = load_or_create_secret(env, name)?;
        let password = load_or_create_password(env, &machine)?;
        let directory = env.store.directory(name)?;
        let answer = directory.join("answer.iso");
        // VirtualBox has no Hyper-V sockets and no TPM, so the agent listens on TCP and Windows 11's checks are bypassed.
        let virtualbox = hv.id() == "virtualbox";
        let mut config = GuestConfig::new(name, tw_proto::SERVICE_GUID, &key, tw_agent::VERSION, uac_off);
        if virtualbox {
            config = config.with_tcp(tw_proto::AGENT_TCP_PORT, !machine.network);
        }
        let request = AnswerRequest {
            options: AnswerOptions {
                image_index: image.index,
                computer_name: computer_name(name),
                password,
                locale,
                bypass_hardware_checks: virtualbox,
            },
            config,
            agent_exe: env.agent_exe.clone(),
        };
        env.say("Building the answer media");
        env.media.build_answer(&request, &answer).await?;
        hv.detach_dvds(&handle).await?;
        hv.attach_dvd(&handle, &iso).await?;
        hv.attach_dvd(&handle, &answer).await?;
        hv.set_boot_dvd(&handle, &iso).await?;
        env.say("Starting the VM; Windows setup runs unattended");
        hv.start(&handle).await?;
        true
    } else {
        ensure_running(env, &machine).await?;
        false
    };

    if started {
        // The Windows boot prompt ("press any key") only waits a few seconds.
        let end = Instant::now() + env.timing.boot_keys;
        while Instant::now() < end {
            let _ = hv.press_key(&handle, HostKey::Space).await;
            tokio::time::sleep(Duration::from_secs(1).min(env.timing.boot_keys)).await;
        }
    }

    if args.flag("detach") {
        return Ok((machine, false));
    }

    let values = wait_ready(env, &machine, env.timing.install).await?;
    hv.detach_dvds(&handle).await?;
    // The answer media holds the account password, so it does not stay on disk.
    let _ = std::fs::remove_file(env.store.directory(name)?.join("answer.iso"));
    machine.installed = true;
    machine.extra.remove("iso");
    machine.extra.remove("image_index");
    env.say("Windows is installed and the agent is running");
    Ok((remember_agent(env, machine, &values)?, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tw_hv::Hypervisor;

    use crate::testing::{Harness, home_preflight, strings};

    #[tokio::test]
    async fn a_fresh_up_installs_and_waits_for_the_agent() {
        let h = Harness::new();
        h.guest.script_unreachable(3);
        let output = up(&h.env, &strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro"])).await.unwrap();
        assert!(output.contains("is ready"), "{output}");

        let machine = h.env.store.load("default").unwrap();
        assert!(machine.installed);
        assert_eq!(machine.agent.as_ref().unwrap().build, 1);
        assert_eq!(machine.guest_version.as_deref(), Some("Windows 11 Pro 24H2 (build 26100)"));
        assert!(!machine.extra.contains_key("iso"));

        let calls = h.hv.calls();
        let position = |needle: &str| {
            calls.iter().position(|c| c.starts_with(needle)).unwrap_or_else(|| panic!("{needle} in {calls:?}"))
        };
        assert!(position("create") < position("attach_dvd") && position("set_boot_dvd") < position("start"));
        assert!(calls.iter().filter(|c| c.starts_with("press_key")).count() >= 1, "{calls:?}");
        let vm = VmHandle::new(&machine.hv_name);
        assert!(h.hv.dvds(&vm).is_empty(), "install media is detached when done");
        assert_eq!(h.media.answer_requests(), vec![("Windows 11 Pro".to_string(), 2)]);
        assert!(h.credentials.has(&machine.credential_target));
        assert!(h.env.store.directory("default").unwrap().join("channel.secret").exists());
        assert!(!h.env.store.directory("default").unwrap().join("answer.iso").exists());
        let progress = h.progress();
        assert!(progress.iter().any(|p| p.contains("Installing Windows")), "{progress:?}");
    }

    fn virtualbox_preflight() -> tw_hv::Preflight {
        tw_hv::Preflight {
            backend: "virtualbox".into(),
            vmms_status: "n/a".into(),
            ..crate::testing::ready_preflight()
        }
    }

    fn home_then_virtualbox() -> Harness {
        Harness::with_backends(vec![
            tw_hv::FakeHypervisor::with_preflight(home_preflight()).with_id("hyperv"),
            tw_hv::FakeHypervisor::with_preflight(virtualbox_preflight()).with_id("virtualbox"),
        ])
    }

    #[tokio::test]
    async fn a_home_pc_falls_back_to_virtualbox_and_sets_up_the_tcp_channel() {
        let h = home_then_virtualbox();
        let args = strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro"]);
        up(&h.env, &args).await.unwrap();
        let machine = h.env.store.load("default").unwrap();
        assert_eq!(machine.backend, "virtualbox");
        assert_eq!(h.media.answer_channels(), vec![("tcp".to_string(), true)]);
        assert!(h.hypervisors[0].calls().iter().all(|c| !c.starts_with("create")), "Hyper-V was not touched");
        assert!(h.hypervisors[1].calls().iter().any(|c| c.starts_with("create")));
        // Later commands use the backend the machine was created with.
        let status = crate::commands::run(&h.env, &strings(&["status"])).await.unwrap();
        assert!(status.contains("default: running"), "{status}");
    }

    #[tokio::test]
    async fn hyper_v_is_preferred_when_it_is_ready() {
        let h = Harness::with_backends(vec![
            tw_hv::FakeHypervisor::default().with_id("hyperv"),
            tw_hv::FakeHypervisor::with_preflight(virtualbox_preflight()).with_id("virtualbox"),
        ]);
        up(&h.env, &strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro"])).await.unwrap();
        assert_eq!(h.env.store.load("default").unwrap().backend, "hyperv");
        assert_eq!(h.media.answer_channels(), vec![("hvsock".to_string(), false)]);
    }

    #[tokio::test]
    async fn the_backend_can_be_chosen_and_a_wrong_choice_is_explained() {
        let h = home_then_virtualbox();
        let base = ["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro"];
        let forced = up(&h.env, &strings(&[&base[..], &["--backend", "hyperv"]].concat())).await.unwrap_err();
        assert_eq!(forced.exit_code(), 3);
        assert!(forced.to_string().contains("Home"), "{forced}");
        let unknown = up(&h.env, &strings(&[&base[..], &["--backend", "qemu"]].concat())).await.unwrap_err();
        assert_eq!(unknown.exit_code(), 2);
        assert!(unknown.to_string().contains("hyperv, virtualbox"), "{unknown}");
        up(&h.env, &strings(&[&base[..], &["--backend", "virtualbox"]].concat())).await.unwrap();
    }

    #[tokio::test]
    async fn when_no_backend_is_ready_every_reason_is_listed() {
        let mut broken = virtualbox_preflight();
        broken.problem = Some("VirtualBox is not installed. Install VirtualBox 7".into());
        let h = Harness::with_backends(vec![
            tw_hv::FakeHypervisor::with_preflight(home_preflight()).with_id("hyperv"),
            tw_hv::FakeHypervisor::with_preflight(broken).with_id("virtualbox"),
        ]);
        let error = up(&h.env, &strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro"])).await.unwrap_err();
        assert_eq!(error.exit_code(), 3);
        let text = error.to_string();
        assert!(
            text.contains("hyperv: ")
                && text.contains("Home")
                && text.contains("virtualbox: ")
                && text.contains("VirtualBox is not installed"),
            "{text}"
        );
        assert!(h.hypervisors.iter().all(|hv| hv.calls().iter().all(|c| !c.starts_with("create"))));
    }

    #[tokio::test]
    async fn no_view_prints_json_when_ready() {
        let h = Harness::new();
        let output =
            up(&h.env, &strings(&["--iso", "C:\\w.iso", "--edition", "Windows 11 Pro", "--no-view"])).await.unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!((json["name"].as_str(), json["state"].as_str()), (Some("default"), Some("ready")));
        assert_eq!(json["agent"]["build"], 1);
    }

    #[tokio::test]
    async fn detach_returns_while_windows_installs_and_a_later_up_finishes() {
        let h = Harness::new();
        h.guest.script_unreachable(10_000);
        let output =
            up(&h.env, &strings(&["--iso", "C:\\w.iso", "--edition", "Windows 11 Pro", "--detach"])).await.unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["state"], "installing");
        assert!(!h.env.store.load("default").unwrap().installed);

        h.guest.script_unreachable(0);
        up(&h.env, &[]).await.unwrap();
        assert!(h.env.store.load("default").unwrap().installed);

        let again = up(&h.env, &strings(&["--detach"])).await.unwrap();
        assert!(again.contains("\"starting\""), "{again}");
    }

    #[test]
    fn the_viewer_opens_only_when_nothing_asks_for_none() {
        assert_eq!(view_target(&[]), Some("default".into()));
        assert_eq!(view_target(&strings(&["work", "--iso", "x.iso"])), Some("work".into()));
        for flag in ["--no-view", "--detach", "--plan"] {
            assert_eq!(view_target(&strings(&[flag])), None, "{flag}");
        }
        assert_eq!(view_target(&strings(&["--bogus"])), None);
    }

    #[tokio::test]
    async fn plan_has_no_side_effects() {
        let h = Harness::new();
        let output = up(&h.env, &strings(&["--plan"])).await.unwrap();
        assert!(output.contains("create-hyper-v-vm"), "{output}");
        assert!(h.hv.calls().is_empty());
        assert!(h.env.store.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_iso_is_required_for_a_new_vm() {
        let h = Harness::new();
        let error = up(&h.env, &[]).await.unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("--iso"));
        assert!(h.hv.calls().iter().all(|c| !c.starts_with("create")));
    }

    #[tokio::test]
    async fn an_unready_host_stops_before_anything_is_created() {
        let h = Harness::with_hypervisor(tw_hv::FakeHypervisor::with_preflight(home_preflight()));
        let error = up(&h.env, &strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro"])).await.unwrap_err();
        assert_eq!(error.exit_code(), 3);
        assert!(error.to_string().contains("Home"));
        assert!(h.hv.calls().iter().all(|c| !c.starts_with("create")));
    }

    #[tokio::test]
    async fn several_editions_need_a_choice_and_nothing_is_created_before_it() {
        let h = Harness::new();
        let error = up(&h.env, &strings(&["--iso", "C:\\win.iso"])).await.unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("--edition"), "{error}");
        assert!(h.hv.calls().iter().all(|c| !c.starts_with("create")));
        assert!(up(&h.env, &strings(&["--iso", "C:\\win.iso", "--image-index", "9"])).await.is_err());
        assert!(
            up(&h.env, &strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro", "--locale", "xx"]))
                .await
                .is_err()
        );
        assert!(
            up(&h.env, &strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro", "--uac", "maybe"]))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_interrupted_install_resumes_without_creating_a_second_vm() {
        let h = Harness::new();
        h.guest.script_unreachable(10_000);
        let first = up(&h.env, &strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro"])).await.unwrap_err();
        assert_eq!(first.exit_code(), 6, "{first}");
        assert!(first.to_string().contains("terminal-windows up default"));
        let machine = h.env.store.load("default").unwrap();
        assert!(!machine.installed);
        assert!(machine.extra.contains_key("iso"), "the ISO is remembered for the retry");

        h.guest.script_unreachable(0);
        up(&h.env, &[]).await.unwrap();
        assert!(h.env.store.load("default").unwrap().installed);
        assert_eq!(h.hv.calls().iter().filter(|c| c.starts_with("create")).count(), 1);
        assert_eq!(h.credentials.count(), 1, "the same password is reused");
    }

    #[tokio::test]
    async fn an_installed_vm_is_only_started() {
        let h = Harness::new();
        up(&h.env, &strings(&["--iso", "C:\\win.iso", "--edition", "Windows 11 Pro"])).await.unwrap();
        let vm = VmHandle::new(h.env.store.load("default").unwrap().hv_name);
        h.hv.stop(&vm, tw_hv::StopMode::Force).await.unwrap();
        let before = h.media.answer_requests().len();
        up(&h.env, &[]).await.unwrap();
        assert_eq!(h.hv.state(&vm).await.unwrap(), VmState::Running);
        assert_eq!(h.media.answer_requests().len(), before, "no new install media");
    }

    #[test]
    fn guest_errors_map_to_exit_codes() {
        let locked = GuestError::Client(ClientError::Remote { code: Some("locked".into()), message: "x".into() });
        assert_eq!(guest_error("a", &locked).exit_code(), 8);
        assert_eq!(guest_error("a", &GuestError::NotCreated("a".into())).exit_code(), 4);
        assert_eq!(
            guest_error("a", &GuestError::NotRunning { name: "a".into(), state: "stopped".into() }).exit_code(),
            5
        );
        let refused = GuestError::Client(ClientError::Connect(std::io::ErrorKind::ConnectionRefused.into()));
        assert_eq!(guest_error("a", &refused).exit_code(), 6);
        assert_eq!(guest_error("a", &GuestError::Other("x".into())).exit_code(), 1);
    }
}
