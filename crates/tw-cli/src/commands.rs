//! Command dispatch. Every command returns the text for stdout; progress goes through `Env::say`.
//! `view`, `mcp` and `setup-host` own the terminal or need Hyper-V directly, so `main` handles them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use tw_client::GuestError;
use tw_core::args::Arguments;
use tw_core::{Error, GuestReadiness, Machine, Result};
use tw_hv::{StopMode, VmHandle, VmState};
use tw_provision::wim;

use crate::env::Env;
use crate::up::{self, ensure_running, guest_error};

pub const HELP: &str = "terminal-windows: a Windows virtual machine you and your AI agents can see and control.

Usage: terminal-windows <command> [name] [options]   (name defaults to 'default')

Get started
  doctor                     Check this PC can run the virtual machine
  setup-host                 One-time elevated host setup (--enable-hyper-v to turn Hyper-V on)
  iso get                    Where to download a Windows ISO
  iso check PATH             List the editions in a Windows ISO
  up [name] --iso PATH       Install Windows unattended and get it ready
                             Options: --edition NAME | --image-index N, --cpus N, --memory GiB, --disk GiB,
                             --width PX, --height PX, --locale en-US, --uac on|off, --offline, --plan,
                             --backend hyperv|virtualbox (default: Hyper-V if ready, else VirtualBox),
                             --detach (return while Windows installs), --no-view (print JSON, no viewer)

Use it
  view [name]                Watch and control the guest in this terminal (Ctrl+G menu, Ctrl+] quits)
  mcp [name] [--config]      Serve the guest to an AI agent over MCP (stdio); --config prints client JSON
  skills get | install       Print or install the agent skill (--dir PATH)
  action ACTION [k=v ...]    Send one action (click x=10 y=20, type text=hi, key key=ctrl+c, ...) [--vm NAME] [--out FILE]
  screenshot [name]          Save a screenshot [--out FILE]
  logs [name]                Show the guest agent log [--lines N]

Manage
  ls                         List virtual machines [--json]
  status [name]              State of one machine [--json]
  display [name]             Show or change the guest display size (VM stopped): --standard | --hidpi | --size WxH
  start | stop | pause | resume [name]    (stop: --force)
  show | hide [name]         Open or close the Hyper-V console window
  checkpoint create|ls|restore|rm [LABEL] [--vm NAME]
  credential show [name]     Print the guest account name and password
  agent update [name]        Update the guest agent from this install
  rm [name] --yes            Delete the machine and its files
  version, help
";

const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn run(env: &Env, args: &[String]) -> Result<String> {
    let Some((command, rest)) = args.split_first() else { return Ok(HELP.to_string()) };
    match command.as_str() {
        "help" | "--help" | "-h" => Ok(HELP.to_string()),
        "version" | "--version" | "-V" => {
            Ok(format!("terminal-windows {} (agent build {})\n", tw_agent::VERSION, tw_agent::BUILD))
        }
        "doctor" => doctor(env, rest).await,
        "iso" => iso(env, rest).await,
        "skills" => skills(rest),
        "display" => display(env, rest).await,
        "up" => up::up(env, rest).await,
        "ls" => ls(env, rest).await,
        "status" => status(env, rest).await,
        "start" => start(env, rest).await,
        "stop" => stop(env, rest).await,
        "pause" => pause(env, rest).await,
        "resume" => resume(env, rest).await,
        "show" => console(env, rest, true).await,
        "hide" => console(env, rest, false).await,
        "rm" => rm(env, rest).await,
        "checkpoint" => checkpoint(env, rest).await,
        "credential" => credential(env, rest),
        "action" => action(env, rest).await,
        "screenshot" => screenshot(env, rest).await,
        "logs" => logs(env, rest).await,
        "agent" => agent(env, rest).await,
        other => Err(Error::Usage(format!("Unknown command: {other}. Run: terminal-windows help"))),
    }
}

fn machine_args(rest: &[String], flags: &[&str], options: &[&str]) -> Result<(Arguments, String)> {
    let args = Arguments::parse(rest, flags, options)?;
    let name = args.machine_name()?;
    Ok((args, name))
}

fn load(env: &Env, name: &str) -> Result<Machine> {
    env.store.load(name)
}

fn handle(machine: &Machine) -> VmHandle {
    VmHandle::new(&machine.hv_name)
}

fn check(ok: bool) -> &'static str {
    if ok { "ok  " } else { "FAIL" }
}

fn backend_lines(p: &tw_hv::Preflight, problem: Option<&str>) -> Vec<String> {
    if p.backend == "virtualbox" {
        return vec![
            format!("{} Windows: {} ({})", check(true), p.caption, p.windows),
            format!("{} VirtualBox: {}", check(problem.is_none()), problem.unwrap_or("installed")),
            format!("     {} logical processors, {} GiB RAM", p.logical_processors, p.ram_bytes >> 30),
        ];
    }
    let can_manage = p.elevated || p.in_hyper_v_administrators;
    vec![
        format!("{} Windows: {} ({})", check(!p.caption.contains("Home")), p.caption, p.windows),
        format!("{} Hypervisor running: {}", check(p.hypervisor_present), p.hypervisor_present),
        format!("{} Hyper-V management service: {}", check(p.vmms_status == "Running"), p.vmms_status),
        format!("{} Hyper-V PowerShell module: {}", check(p.hyper_v_module), p.hyper_v_module),
        format!("{} Permission (elevated or Hyper-V Administrators): {can_manage}", check(can_manage)),
        format!("{} Guest control channel registered: {}", check(p.service_registered), p.service_registered),
        format!("{} Hyper-V console (vmconnect): {}", check(p.vmconnect), p.vmconnect),
        format!("     {} logical processors, {} GiB RAM", p.logical_processors, p.ram_bytes >> 30),
    ]
}

async fn doctor(env: &Env, rest: &[String]) -> Result<String> {
    let args = Arguments::parse(rest, &["json"], &[])?;
    let mut lines = Vec::new();
    let mut backends = Vec::new();
    let mut problems = Vec::new();
    let mut ready = false;
    for hv in &env.hypervisors {
        let outcome = match hv.preflight().await {
            Ok(p) => {
                let verdict = p.check_ready();
                backends.push(json!({ "backend": hv.id(), "ready": verdict.is_ok(), "host": p, "problem": verdict.as_ref().err().map(|e| e.to_string()) }));
                lines.push(format!("Backend: {}", hv.id()));
                lines.extend(backend_lines(
                    &p,
                    verdict
                        .as_ref()
                        .err()
                        .map(|_| p.problem.as_deref().unwrap_or("not ready"))
                        .filter(|_| p.backend == "virtualbox"),
                ));
                verdict
            }
            Err(error) => {
                backends.push(json!({ "backend": hv.id(), "ready": false, "problem": error.to_string() }));
                lines.push(format!("Backend: {}\nFAIL {error}", hv.id()));
                Err(error)
            }
        };
        match outcome {
            Ok(()) => ready = true,
            Err(error) => problems.push(format!("{}: {error}", hv.id())),
        }
    }
    let agent_present = env.agent_exe.is_file();
    if args.flag("json") {
        let text = serde_json::to_string_pretty(&json!({ "ready": ready && agent_present, "backends": backends }))?;
        return Ok(text + "\n");
    }
    lines.push(format!("{} Guest agent file: {}", check(agent_present), env.agent_exe.display()));
    match (ready, agent_present) {
        (true, true) => {
            lines.push("This PC is ready. Next: terminal-windows up --iso PATH".into());
            Ok(lines.join("\n") + "\n")
        }
        (true, false) => {
            lines.push("The guest agent (twagent.exe) must sit next to terminal-windows.exe. Reinstall or set TERMINAL_WINDOWS_AGENT.".into());
            Err(Error::Prerequisite(lines.join("\n")))
        }
        (false, _) => {
            lines.push(format!("Not ready: {}", problems.join("; ")));
            Err(Error::Prerequisite(lines.join("\n")))
        }
    }
}

async fn vm_state(env: &Env, machine: &Machine) -> Result<VmState> {
    env.hv_for(machine)?.state(&handle(machine)).await
}

const ISO_USAGE: &str = "Usage: terminal-windows iso check PATH | iso get";

const ISO_DOWNLOAD_HELP: &str = "Download a Windows ISO from Microsoft (free; no product key is needed to install):
  Windows 11: https://www.microsoft.com/software-download/windows11
  Windows 10: https://www.microsoft.com/software-download/windows10
Pick the 64-bit (x64) multi-edition ISO, then check it with: terminal-windows iso check PATH
";

async fn iso(env: &Env, rest: &[String]) -> Result<String> {
    let Some((sub, rest)) = rest.split_first() else {
        return Err(Error::Usage(ISO_USAGE.into()));
    };
    if sub == "get" {
        return Ok(ISO_DOWNLOAD_HELP.to_string());
    }
    if sub != "check" {
        return Err(Error::Usage(ISO_USAGE.into()));
    }
    let args = Arguments::parse(rest, &[], &["edition", "image-index"])?;
    let [path] = args.positionals.as_slice() else {
        return Err(Error::Usage("Usage: terminal-windows iso check PATH".into()));
    };
    let info = env.media.inspect(&PathBuf::from(path)).await?;
    let image_index = match args.option("image-index") {
        Some(_) => Some(args.integer("image-index", 1, 1..=64)? as u32),
        None => None,
    };
    let mut text = format!("{} holds {} image(s) in {}:\n", path, info.images.len(), info.image_file);
    for image in &info.images {
        text.push_str(&format!("  {}: {} (build {})\n", image.index, image.name, image.build));
        if let Some(warning) = image.support_warning() {
            text.push_str(&format!("     Warning: {warning}\n"));
        }
    }
    match wim::select(&info.images, image_index, args.option("edition")) {
        Ok(chosen) => text.push_str(&format!("Selected: {} (image {})\n", chosen.name, chosen.index)),
        Err(error) if args.option("edition").is_some() || image_index.is_some() => return Err(error),
        Err(error) => text.push_str(&format!("{error}\n")),
    }
    Ok(text)
}

const SKILL: &str = include_str!("../../../skills/terminal-windows/SKILL.md");

/// `<home>\.claude\skills`, where Claude Code looks for personal skills.
fn default_skills_dir() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| Error::Prerequisite("Cannot find your home folder. Use --dir PATH".into()))?;
    Ok(PathBuf::from(home).join(".claude").join("skills"))
}

/// Checked-out files may have CRLF endings; the installed copy always uses LF.
fn skill_text() -> String {
    SKILL.replace("\r\n", "\n")
}

fn skills(rest: &[String]) -> Result<String> {
    let usage = || Error::Usage("Usage: terminal-windows skills get | install [--dir PATH]".into());
    let Some((sub, rest)) = rest.split_first() else { return Err(usage()) };
    match sub.as_str() {
        "get" => Ok(skill_text()),
        "install" => {
            let args = Arguments::parse(rest, &[], &["dir"])?;
            let base = match args.option("dir") {
                Some(dir) => PathBuf::from(dir),
                None => default_skills_dir()?,
            };
            let folder = base.join("terminal-windows");
            std::fs::create_dir_all(&folder)?;
            std::fs::write(folder.join("SKILL.md"), skill_text())?;
            Ok(format!("Installed the agent skill to {}\n", folder.join("SKILL.md").display()))
        }
        _ => Err(usage()),
    }
}

/// The MCP server entry for clients that take a JSON config (Cursor, VS Code, Codex, ...).
pub fn mcp_config(name: &str) -> Result<String> {
    let (key, args) = if name == "default" {
        ("terminal-windows".to_string(), vec!["mcp".to_string()])
    } else {
        (format!("terminal-windows-{name}"), vec!["mcp".to_string(), name.to_string()])
    };
    let config = json!({ "mcpServers": { key: { "command": "terminal-windows", "args": args } } });
    Ok(serde_json::to_string_pretty(&config)? + "\n")
}

async fn ls(env: &Env, rest: &[String]) -> Result<String> {
    let args = Arguments::parse(rest, &["json"], &[])?;
    let machines = env.store.list()?;
    let mut rows = Vec::new();
    for machine in &machines {
        let state = vm_state(env, machine).await.map_or_else(|_| "missing".to_string(), |s| s.as_str().to_string());
        rows.push((machine, state));
    }
    if args.flag("json") {
        let items: Vec<_> = rows
            .iter()
            .map(|(m, state)| json!({ "name": m.name, "state": state, "installed": m.installed, "cpu": m.cpu, "memoryGiB": m.memory_gib, "diskGiB": m.disk_gib }))
            .collect();
        return Ok(serde_json::to_string_pretty(&items)? + "\n");
    }
    if rows.is_empty() {
        return Ok("No virtual machines yet. Create one with: terminal-windows up --iso PATH\n".into());
    }
    let mut text = String::from("NAME            STATE      INSTALLED  CPU  MEMORY  DISK\n");
    for (m, state) in rows {
        let installed = if m.installed { "yes" } else { "no" };
        let (memory, disk) = (format!("{} GiB", m.memory_gib), format!("{} GiB", m.disk_gib));
        text.push_str(&format!("{:<15} {state:<10} {installed:<10} {:<4} {memory:<7} {disk}\n", m.name, m.cpu));
    }
    Ok(text)
}

fn parse_size(text: &str) -> Result<(u32, u32)> {
    let parsed = text.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)));
    match parsed {
        Some((w, h)) if (800..=3840).contains(&w) && (600..=2160).contains(&h) => Ok((w, h)),
        _ => Err(Error::Usage("--size must look like 1920x1080 (800x600 up to 3840x2160)".into())),
    }
}

async fn display(env: &Env, rest: &[String]) -> Result<String> {
    let (args, name) = machine_args(rest, &["standard", "hidpi"], &["size"])?;
    let mut machine = load(env, &name)?;
    let requested = [args.flag("standard"), args.flag("hidpi"), args.option("size").is_some()];
    if requested.iter().filter(|r| **r).count() > 1 {
        return Err(Error::Usage("Choose one of --standard, --hidpi or --size WxH".into()));
    }
    let size = if args.flag("standard") {
        Some((1280, 800))
    } else if args.flag("hidpi") {
        Some((2560, 1600))
    } else {
        args.option("size").map(parse_size).transpose()?
    };
    let Some((width, height)) = size else {
        return Ok(format!("{name}: {}x{}\n", machine.width, machine.height));
    };
    let hv = env.hv_for(&machine)?;
    if hv.state(&handle(&machine)).await? != VmState::Off {
        return Err(Error::Other(format!("Stop '{name}' first: terminal-windows stop {name}")));
    }
    hv.set_display(&handle(&machine), width, height).await?;
    machine.width = width;
    machine.height = height;
    env.store.save(&machine)?;
    Ok(format!("Display set to {width}x{height}. It applies the next time '{name}' starts.\n"))
}

async fn status(env: &Env, rest: &[String]) -> Result<String> {
    let (args, name) = machine_args(rest, &["json"], &[])?;
    let machine = load(env, &name)?;
    let state = vm_state(env, &machine).await?;
    let mut values = BTreeMap::new();
    let mut readiness = GuestReadiness::Unavailable;
    if state == VmState::Running {
        if let Ok(reply) = (env.guest)(&name).call("status", BTreeMap::new(), Duration::from_secs(5)).await {
            readiness = GuestReadiness::from_session(reply.values.get("session").map(String::as_str));
            values = reply.values;
        }
    }
    if args.flag("json") {
        let text = serde_json::to_string_pretty(&json!({
            "name": machine.name, "state": state.as_str(), "installed": machine.installed, "guest": readiness,
            "agent": values.get("version"), "build": values.get("build"), "os": values.get("os"),
            "width": values.get("width"), "height": values.get("height"),
        }))?;
        return Ok(text + "\n");
    }
    let mut text = format!("{}: {}\n", machine.name, state.as_str());
    text.push_str(&format!(
        "  installed: {}\n",
        if machine.installed { "yes" } else { "no (run: terminal-windows up)" }
    ));
    text.push_str(&format!("  guest: {}\n", serde_json::to_value(readiness)?.as_str().unwrap_or("unknown")));
    if let Some(version) = values.get("version") {
        text.push_str(&format!("  agent: {version} (build {})\n", values.get("build").map_or("?", String::as_str)));
    }
    if let Some(os) = values.get("os") {
        text.push_str(&format!("  os: {os}\n"));
    }
    if let (Some(w), Some(h)) = (values.get("width"), values.get("height")) {
        text.push_str(&format!("  display: {w}x{h}\n"));
    }
    Ok(text)
}

async fn start(env: &Env, rest: &[String]) -> Result<String> {
    let (_, name) = machine_args(rest, &[], &[])?;
    let machine = load(env, &name)?;
    let started = ensure_running(env, &machine).await?;
    Ok(if started { format!("Started '{name}'\n") } else { format!("'{name}' is already running\n") })
}

async fn stop(env: &Env, rest: &[String]) -> Result<String> {
    let (args, name) = machine_args(rest, &["force"], &[])?;
    let machine = load(env, &name)?;
    let hv = env.hv_for(&machine)?;
    let mode = if args.flag("force") { StopMode::Force } else { StopMode::Graceful };
    if hv.state(&handle(&machine)).await? == VmState::Off {
        return Ok(format!("'{name}' is already stopped\n"));
    }
    hv.stop(&handle(&machine), mode).await?;
    Ok(format!("Stopped '{name}'\n"))
}

async fn pause(env: &Env, rest: &[String]) -> Result<String> {
    let (_, name) = machine_args(rest, &[], &[])?;
    let machine = load(env, &name)?;
    let hv = env.hv_for(&machine)?;
    match hv.state(&handle(&machine)).await? {
        VmState::Paused => Ok(format!("'{name}' is already paused\n")),
        VmState::Running => {
            hv.pause(&handle(&machine)).await?;
            Ok(format!("Paused '{name}'\n"))
        }
        _ => Err(Error::NotRunning(name)),
    }
}

async fn resume(env: &Env, rest: &[String]) -> Result<String> {
    let (_, name) = machine_args(rest, &[], &[])?;
    let machine = load(env, &name)?;
    let hv = env.hv_for(&machine)?;
    match hv.state(&handle(&machine)).await? {
        VmState::Paused => {
            hv.resume(&handle(&machine)).await?;
            Ok(format!("Resumed '{name}'\n"))
        }
        VmState::Running => Ok(format!("'{name}' is already running\n")),
        _ => Err(Error::NotRunning(name)),
    }
}

async fn console(env: &Env, rest: &[String], open: bool) -> Result<String> {
    let (_, name) = machine_args(rest, &[], &[])?;
    let machine = load(env, &name)?;
    let hv = env.hv_for(&machine)?;
    if open {
        hv.open_console(&handle(&machine)).await?;
        Ok(format!("Opened the Hyper-V console for '{name}'\n"))
    } else {
        hv.close_console(&handle(&machine)).await?;
        Ok(format!("Closed the Hyper-V console for '{name}'\n"))
    }
}

async fn rm(env: &Env, rest: &[String]) -> Result<String> {
    let (args, name) = machine_args(rest, &["yes"], &[])?;
    let machine = load(env, &name)?;
    let hv = env.hv_for(&machine)?;
    if !args.flag("yes") {
        return Err(Error::Usage(format!(
            "This deletes '{name}', its disk and its checkpoints. Run again with --yes to confirm: terminal-windows rm {name} --yes"
        )));
    }
    let vm = handle(&machine);
    if hv.state(&vm).await? != VmState::Off {
        hv.stop(&vm, StopMode::Force).await?;
    }
    let _ = hv.close_console(&vm).await;
    hv.remove(&vm).await?;
    let directory = env.store.directory(&name)?;
    if directory.exists() {
        std::fs::remove_dir_all(&directory)?;
    }
    env.credentials.remove(&machine.credential_target)?;
    Ok(format!("Deleted '{name}'\n"))
}

async fn checkpoint(env: &Env, rest: &[String]) -> Result<String> {
    let usage = || Error::Usage("Usage: terminal-windows checkpoint create|ls|restore|rm [LABEL] [--vm NAME]".into());
    let Some((sub, rest)) = rest.split_first() else { return Err(usage()) };
    let args = Arguments::parse(rest, &[], &["vm"])?;
    let name = args.option("vm").unwrap_or("default").to_string();
    tw_core::validate_name(&name)?;
    let machine = load(env, &name)?;
    let hv = env.hv_for(&machine)?;
    let vm = handle(&machine);
    let label = |args: &Arguments| match args.positionals.as_slice() {
        [label] => Ok(label.clone()),
        _ => Err(Error::Usage("Provide one checkpoint LABEL".into())),
    };
    match sub.as_str() {
        "create" => {
            let label = label(&args)?;
            hv.checkpoint_create(&vm, &label).await?;
            Ok(format!("Created checkpoint '{label}'\n"))
        }
        "ls" => {
            let items = hv.checkpoint_list(&vm).await?;
            if items.is_empty() {
                return Ok("No checkpoints\n".into());
            }
            Ok(items.iter().map(|c| format!("{}  {}\n", c.name, c.created)).collect())
        }
        "restore" => {
            let label = label(&args)?;
            hv.checkpoint_restore(&vm, &label).await?;
            Ok(format!("Restored checkpoint '{label}'. Run: terminal-windows up {name}\n"))
        }
        "rm" => {
            let label = label(&args)?;
            hv.checkpoint_remove(&vm, &label).await?;
            Ok(format!("Removed checkpoint '{label}'\n"))
        }
        _ => Err(usage()),
    }
}

fn credential(env: &Env, rest: &[String]) -> Result<String> {
    let Some((sub, rest)) = rest.split_first() else {
        return Err(Error::Usage("Usage: terminal-windows credential show [name]".into()));
    };
    if sub != "show" {
        return Err(Error::Usage("Usage: terminal-windows credential show [name]".into()));
    }
    let (_, name) = machine_args(rest, &[], &[])?;
    let machine = load(env, &name)?;
    match env.credentials.fetch(&machine.credential_target)? {
        Some((user, password)) => Ok(format!("user: {user}\npassword: {password}\n")),
        None => Err(Error::Other(format!("No stored credential for '{name}'"))),
    }
}

async fn action(env: &Env, rest: &[String]) -> Result<String> {
    let args = Arguments::parse(rest, &[], &["vm", "out"])?;
    let name = args.option("vm").unwrap_or("default").to_string();
    tw_core::validate_name(&name)?;
    let Some((action, pairs)) = args.positionals.split_first() else {
        return Err(Error::Usage(
            "Usage: terminal-windows action ACTION [key=value ...] [--vm NAME] [--out FILE]".into(),
        ));
    };
    let mut values = BTreeMap::new();
    for pair in pairs {
        let (key, value) =
            pair.split_once('=').ok_or_else(|| Error::Usage(format!("Arguments look like key=value, got '{pair}'")))?;
        values.insert(key.to_string(), value.to_string());
    }
    let reply = (env.guest)(&name).call(action, values, ACTION_TIMEOUT).await.map_err(|e| guest_error(&name, &e))?;
    let mut text = serde_json::to_string_pretty(&reply.values)? + "\n";
    if !reply.payload.is_empty() {
        match args.option("out") {
            Some(out) => {
                std::fs::write(out, &reply.payload)?;
                text.push_str(&format!("Wrote {} bytes to {out}\n", reply.payload.len()));
            }
            None if action == "logs" => text = String::from_utf8_lossy(&reply.payload).into_owned() + "\n",
            None => text.push_str(&format!("({} bytes of data; use --out FILE to save it)\n", reply.payload.len())),
        }
    }
    Ok(text)
}

async fn screenshot(env: &Env, rest: &[String]) -> Result<String> {
    let (args, name) = machine_args(rest, &[], &["out"])?;
    let out = args.option("out").unwrap_or("screenshot.png").to_string();
    let reply = (env.guest)(&name)
        .call("screenshot", BTreeMap::new(), ACTION_TIMEOUT)
        .await
        .map_err(|e| guest_error(&name, &e))?;
    std::fs::write(&out, &reply.payload)?;
    Ok(format!(
        "Saved {}x{} screenshot to {out}\n",
        reply.values.get("width").map_or("?", String::as_str),
        reply.values.get("height").map_or("?", String::as_str)
    ))
}

async fn logs(env: &Env, rest: &[String]) -> Result<String> {
    let (args, name) = machine_args(rest, &[], &["lines"])?;
    let lines = args.integer("lines", 200, 1..=2000)?;
    let params = BTreeMap::from([("lines".to_string(), lines.to_string())]);
    let reply = (env.guest)(&name).call("logs", params, ACTION_TIMEOUT).await.map_err(|e| guest_error(&name, &e))?;
    Ok(String::from_utf8_lossy(&reply.payload).into_owned() + "\n")
}

async fn agent(env: &Env, rest: &[String]) -> Result<String> {
    let usage = || Error::Usage("Usage: terminal-windows agent update [name]".into());
    let Some((sub, rest)) = rest.split_first() else { return Err(usage()) };
    if sub != "update" {
        return Err(usage());
    }
    let (_, name) = machine_args(rest, &[], &[])?;
    let guest = (env.guest)(&name);
    let status =
        guest.call("status", BTreeMap::new(), Duration::from_secs(5)).await.map_err(|e| guest_error(&name, &e))?;
    let running: u32 = status.values.get("build").and_then(|b| b.parse().ok()).unwrap_or(0);
    let bundled = tw_agent::BUILD;
    if running >= bundled {
        return Ok(format!("The guest agent is up to date (build {running})\n"));
    }
    let bytes = std::fs::read(&env.agent_exe)
        .map_err(|e| Error::Other(format!("Cannot read the bundled agent {}: {e}", env.agent_exe.display())))?;
    let params = BTreeMap::from([
        ("sha256".to_string(), tw_agent::update::sha256_hex(&bytes)),
        ("build".to_string(), bundled.to_string()),
        ("size".to_string(), bytes.len().to_string()),
    ]);
    guest.upload("update", params, bytes, Duration::from_secs(120)).await.map_err(|e| match &e {
        GuestError::Client(tw_client::ClientError::Remote { message, .. }) => {
            Error::Other(format!("The guest refused the update: {message}"))
        }
        _ => guest_error(&name, &e),
    })?;
    Ok(format!("Updated the guest agent from build {running} to {bundled}. It restarts in a few seconds.\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Harness, home_preflight, strings};

    async fn installed() -> Harness {
        let h = Harness::new();
        run(&h.env, &strings(&["up", "--iso", "C:\\w.iso", "--edition", "Windows 11 Pro"])).await.unwrap();
        h
    }

    #[tokio::test]
    async fn help_and_version_and_unknown_commands() {
        let h = Harness::new();
        assert!(run(&h.env, &[]).await.unwrap().contains("Usage"));
        assert!(run(&h.env, &strings(&["version"])).await.unwrap().starts_with("terminal-windows 0.1.2"));
        let error = run(&h.env, &strings(&["frobnicate"])).await.unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }

    #[tokio::test]
    async fn doctor_reports_ready_and_not_ready_hosts() {
        let h = Harness::new();
        let ok = run(&h.env, &strings(&["doctor"])).await.unwrap();
        assert!(ok.contains("This PC is ready"), "{ok}");
        let json = run(&h.env, &strings(&["doctor", "--json"])).await.unwrap();
        assert!(json.contains("\"ready\": true"), "{json}");

        let home = Harness::with_hypervisor(tw_hv::FakeHypervisor::with_preflight(home_preflight()));
        let error = run(&home.env, &strings(&["doctor"])).await.unwrap_err();
        assert_eq!(error.exit_code(), 3);
        assert!(error.to_string().contains("FAIL") && error.to_string().contains("Home"));

        std::fs::remove_file(&h.env.agent_exe).unwrap();
        let missing = run(&h.env, &strings(&["doctor"])).await.unwrap_err();
        assert!(missing.to_string().contains("twagent.exe"));
    }

    #[tokio::test]
    async fn doctor_is_ready_when_any_backend_is_ready_and_names_each_one() {
        let h = Harness::with_backends(vec![
            tw_hv::FakeHypervisor::with_preflight(home_preflight()).with_id("hyperv"),
            tw_hv::FakeHypervisor::with_preflight(tw_hv::Preflight {
                backend: "virtualbox".into(),
                ..crate::testing::ready_preflight()
            })
            .with_id("virtualbox"),
        ]);
        let text = run(&h.env, &strings(&["doctor"])).await.unwrap();
        assert!(
            text.contains("Backend: hyperv")
                && text.contains("Backend: virtualbox")
                && text.contains("VirtualBox: installed"),
            "{text}"
        );
        assert!(text.contains("This PC is ready"));
        let json: serde_json::Value =
            serde_json::from_str(&run(&h.env, &strings(&["doctor", "--json"])).await.unwrap()).unwrap();
        assert_eq!(json["ready"], true);
        assert_eq!(json["backends"][0]["ready"], false);
        assert_eq!(json["backends"][1]["ready"], true);
    }

    #[tokio::test]
    async fn the_display_size_is_shown_and_changed_only_on_a_stopped_vm() {
        let h = installed().await;
        assert_eq!(run(&h.env, &strings(&["display"])).await.unwrap(), "default: 1280x800\n");
        let running = run(&h.env, &strings(&["display", "--hidpi"])).await.unwrap_err();
        assert!(running.to_string().contains("Stop 'default' first"), "{running}");
        run(&h.env, &strings(&["stop", "--force"])).await.unwrap();
        run(&h.env, &strings(&["display", "--hidpi"])).await.unwrap();
        assert_eq!(h.env.store.load("default").unwrap().width, 2560);
        let vm = VmHandle::new(h.env.store.load("default").unwrap().hv_name);
        assert_eq!(h.hv.display(&vm), Some((2560, 1600)));
        run(&h.env, &strings(&["display", "--size", "1920x1080"])).await.unwrap();
        assert_eq!(run(&h.env, &strings(&["display"])).await.unwrap(), "default: 1920x1080\n");
        run(&h.env, &strings(&["display", "--standard"])).await.unwrap();
        assert_eq!(h.env.store.load("default").unwrap().height, 800);
    }

    #[tokio::test]
    async fn bad_display_requests_are_usage_errors() {
        let h = installed().await;
        run(&h.env, &strings(&["stop", "--force"])).await.unwrap();
        for args in
            [&["display", "--size", "10x10"][..], &["display", "--size", "big"], &["display", "--hidpi", "--standard"]]
        {
            assert_eq!(run(&h.env, &strings(args)).await.unwrap_err().exit_code(), 2, "{args:?}");
        }
    }

    #[tokio::test]
    async fn skills_are_printed_and_installed() {
        let h = Harness::new();
        let text = run(&h.env, &strings(&["skills", "get"])).await.unwrap();
        assert!(text.starts_with("---\nname: terminal-windows"), "{text}");
        let dir = h.dir.path().join("skills");
        let done = run(&h.env, &strings(&["skills", "install", "--dir", dir.to_str().unwrap()])).await.unwrap();
        assert!(done.contains("SKILL.md"));
        assert_eq!(std::fs::read_to_string(dir.join("terminal-windows").join("SKILL.md")).unwrap(), text);
        assert!(run(&h.env, &strings(&["skills"])).await.is_err());
        assert!(run(&h.env, &strings(&["skills", "bogus"])).await.is_err());
    }

    #[test]
    fn mcp_config_names_the_machine_when_it_is_not_the_default() {
        let default: serde_json::Value = serde_json::from_str(&mcp_config("default").unwrap()).unwrap();
        assert_eq!(default["mcpServers"]["terminal-windows"]["args"], json!(["mcp"]));
        let work: serde_json::Value = serde_json::from_str(&mcp_config("work").unwrap()).unwrap();
        assert_eq!(work["mcpServers"]["terminal-windows-work"]["args"], json!(["mcp", "work"]));
        assert_eq!(work["mcpServers"]["terminal-windows-work"]["command"], "terminal-windows");
    }

    #[tokio::test]
    async fn iso_get_points_at_microsoft() {
        let h = Harness::new();
        let text = run(&h.env, &strings(&["iso", "get"])).await.unwrap();
        assert!(text.contains("microsoft.com/software-download/windows11") && text.contains("iso check"));
    }

    #[tokio::test]
    async fn iso_check_lists_editions() {
        let h = Harness::new();
        let text = run(&h.env, &strings(&["iso", "check", "C:\\w.iso"])).await.unwrap();
        assert!(text.contains("1: Windows 11 Home") && text.contains("2: Windows 11 Pro"), "{text}");
        assert!(text.contains("--edition"), "several editions explain the choice: {text}");
        let chosen =
            run(&h.env, &strings(&["iso", "check", "C:\\w.iso", "--edition", "Windows 11 Pro"])).await.unwrap();
        assert!(chosen.contains("Selected: Windows 11 Pro"));
        assert!(run(&h.env, &strings(&["iso", "check", "C:\\w.iso", "--edition", "Nope"])).await.is_err());
        assert!(run(&h.env, &strings(&["iso", "check"])).await.is_err());
    }

    #[tokio::test]
    async fn ls_and_status_describe_the_machine() {
        let h = Harness::new();
        assert!(run(&h.env, &strings(&["ls"])).await.unwrap().contains("No virtual machines"));
        let h = installed().await;
        let list = run(&h.env, &strings(&["ls"])).await.unwrap();
        assert!(list.contains("default") && list.contains("running") && list.contains("yes"), "{list}");
        let json: serde_json::Value =
            serde_json::from_str(&run(&h.env, &strings(&["ls", "--json"])).await.unwrap()).unwrap();
        assert_eq!(json[0]["name"], "default");
        let status = run(&h.env, &strings(&["status"])).await.unwrap();
        assert!(
            status.contains("guest: ready")
                && status.contains("agent: 0.1.0 (build 1)")
                && status.contains("display: 1280x800"),
            "{status}"
        );
        let json: serde_json::Value =
            serde_json::from_str(&run(&h.env, &strings(&["status", "--json"])).await.unwrap()).unwrap();
        assert_eq!(json["guest"], "ready");
        assert_eq!(run(&h.env, &strings(&["status", "missing"])).await.unwrap_err().exit_code(), 4);
    }

    #[tokio::test]
    async fn lifecycle_commands_follow_the_vm_state() {
        let h = installed().await;
        assert!(run(&h.env, &strings(&["start"])).await.unwrap().contains("already running"));
        assert!(run(&h.env, &strings(&["pause"])).await.unwrap().contains("Paused"));
        assert!(run(&h.env, &strings(&["pause"])).await.unwrap().contains("already paused"));
        assert!(run(&h.env, &strings(&["resume"])).await.unwrap().contains("Resumed"));
        assert!(run(&h.env, &strings(&["stop", "--force"])).await.unwrap().contains("Stopped"));
        assert!(run(&h.env, &strings(&["stop"])).await.unwrap().contains("already stopped"));
        assert_eq!(run(&h.env, &strings(&["pause"])).await.unwrap_err().exit_code(), 5);
        assert_eq!(run(&h.env, &strings(&["resume"])).await.unwrap_err().exit_code(), 5);
        assert!(run(&h.env, &strings(&["start"])).await.unwrap().contains("Started"));
        run(&h.env, &strings(&["show"])).await.unwrap();
        run(&h.env, &strings(&["hide"])).await.unwrap();
    }

    #[tokio::test]
    async fn rm_needs_confirmation_and_cleans_everything() {
        let h = installed().await;
        let target = h.env.store.load("default").unwrap().credential_target;
        assert_eq!(run(&h.env, &strings(&["rm"])).await.unwrap_err().exit_code(), 2);
        assert!(h.env.store.load("default").is_ok(), "nothing was deleted without --yes");
        run(&h.env, &strings(&["rm", "--yes"])).await.unwrap();
        assert!(h.env.store.load("default").is_err());
        assert!(!h.env.store.directory("default").unwrap().exists());
        assert!(!h.credentials.has(&target));
        assert!(h.hv.calls().iter().any(|c| c.starts_with("remove ")));
    }

    #[tokio::test]
    async fn checkpoints_round_trip() {
        let h = installed().await;
        assert!(run(&h.env, &strings(&["checkpoint", "ls"])).await.unwrap().contains("No checkpoints"));
        run(&h.env, &strings(&["checkpoint", "create", "clean"])).await.unwrap();
        assert!(run(&h.env, &strings(&["checkpoint", "ls"])).await.unwrap().contains("clean"));
        run(&h.env, &strings(&["checkpoint", "restore", "clean"])).await.unwrap();
        run(&h.env, &strings(&["checkpoint", "rm", "clean"])).await.unwrap();
        assert!(run(&h.env, &strings(&["checkpoint", "create"])).await.is_err());
        assert!(run(&h.env, &strings(&["checkpoint", "bogus"])).await.is_err());
        assert!(run(&h.env, &strings(&["checkpoint"])).await.is_err());
    }

    #[tokio::test]
    async fn credentials_are_shown_only_on_request() {
        let h = installed().await;
        let text = run(&h.env, &strings(&["credential", "show"])).await.unwrap();
        assert!(text.starts_with("user: agent\npassword: ") && text.trim_end().len() > 30, "{text}");
        assert!(run(&h.env, &strings(&["credential"])).await.is_err());
    }

    #[tokio::test]
    async fn actions_reach_the_guest() {
        let h = installed().await;
        let text = run(&h.env, &strings(&["action", "click", "x=10", "y=20"])).await.unwrap();
        assert!(text.contains("\"ok\": \"true\""));
        let calls = h.guest.calls.lock().unwrap().clone();
        assert_eq!(calls[0].0, "click");
        assert_eq!(calls[0].1["x"], "10");
        assert!(run(&h.env, &strings(&["action", "click", "x10"])).await.is_err());
        assert!(run(&h.env, &strings(&["action"])).await.is_err());

        *h.guest.fail_with.lock().unwrap() = Some("bad thing".into());
        let error = run(&h.env, &strings(&["action", "click", "x=1", "y=1"])).await.unwrap_err();
        assert!(error.to_string().contains("bad thing"));
    }

    #[tokio::test]
    async fn screenshots_and_logs_are_saved_or_printed() {
        let h = installed().await;
        let out = h.dir.path().join("shot.png");
        let text = run(&h.env, &strings(&["screenshot", "--out", out.to_str().unwrap()])).await.unwrap();
        assert!(text.contains("Saved"));
        assert_eq!(std::fs::read(&out).unwrap(), b"\x89PNG-fake");
        let logs = run(&h.env, &strings(&["logs", "--lines", "2"])).await.unwrap();
        assert!(logs.contains("line one"));
        assert!(run(&h.env, &strings(&["logs", "--lines", "9999"])).await.is_err());
        let from_action = run(&h.env, &strings(&["action", "logs", "lines=2"])).await.unwrap();
        assert!(from_action.contains("line two"));
    }

    #[tokio::test]
    async fn agent_update_uploads_only_when_the_guest_is_behind() {
        let h = installed().await;
        assert!(run(&h.env, &strings(&["agent", "update"])).await.unwrap().contains("up to date"));
        assert!(h.guest.uploads.lock().unwrap().is_empty());

        *h.guest.build.lock().unwrap() = 0;
        let text = run(&h.env, &strings(&["agent", "update"])).await.unwrap();
        assert!(text.contains("Updated"), "{text}");
        let uploads = h.guest.uploads.lock().unwrap().clone();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].0, "update");
        assert_eq!(uploads[0].1["size"], "2048");
        assert_eq!(uploads[0].1["sha256"].len(), 64);
        assert_eq!(uploads[0].2, 2048);
        assert!(run(&h.env, &strings(&["agent"])).await.is_err());
    }

    #[tokio::test]
    async fn commands_on_a_missing_vm_say_how_to_create_one() {
        let h = Harness::new();
        for command in ["start", "stop", "pause", "resume", "show", "rm", "credential show"] {
            let mut args = strings(&command.split(' ').collect::<Vec<_>>());
            if command == "rm" {
                args.push("--yes".into());
            }
            assert_eq!(run(&h.env, &args).await.unwrap_err().exit_code(), 4, "{command}");
        }
    }
}
