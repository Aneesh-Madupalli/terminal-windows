#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! `twagent run | supervise | self-check | --version`

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use tw_agent::{BUILD, VERSION, config, guard, log::FileLog};

fn data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    base.join("terminal-windows")
}

fn log() -> Arc<FileLog> {
    Arc::new(FileLog::new(data_dir().join("agent.log")))
}

#[cfg(windows)]
fn main() -> ExitCode {
    use tw_agent::win;

    win::attach_parent_console();
    win::init_process();
    let command = std::env::args().nth(1).unwrap_or_default();
    match command.as_str() {
        "--version" | "version" => {
            println!("twagent {VERSION} (build {BUILD})");
            ExitCode::SUCCESS
        }
        "self-check" => self_check(),
        "run" => run(),
        "supervise" => supervise(),
        _ => {
            println!("twagent {VERSION}\nUsage: twagent run | supervise | self-check | --version");
            ExitCode::from(2)
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("twagent {VERSION} runs only inside a Windows guest");
    ExitCode::from(1)
}

#[cfg(windows)]
fn self_check() -> ExitCode {
    use tw_agent::desktop::Desktop;
    use tw_agent::win::WinDesktop;

    let mut failed = false;
    let mut report = |name: &str, result: Result<String, String>| match result {
        Ok(detail) => println!("ok    {name}: {detail}"),
        Err(detail) => {
            failed = true;
            println!("FAIL  {name}: {detail}");
        }
    };
    report("Hyper-V guest", guard::check().map(|()| "yes".into()));
    report(
        "configuration",
        config::load(&data_dir().join("agent.json"))
            .map(|c| format!("service {}", c.service_id))
            .map_err(|e| e.to_string()),
    );
    let desktop = WinDesktop;
    report("session", Ok(desktop.session().as_str().into()));
    report(
        "screen",
        desktop.screen().map(|s| format!("{}x{} at {} dpi", s.width, s.height, s.dpi)).map_err(|e| e.to_string()),
    );
    report("capture", desktop.capture().map(|b| format!("{}x{}", b.width, b.height)).map_err(|e| e.to_string()));
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

#[cfg(windows)]
fn run() -> ExitCode {
    use tw_agent::config::Channel;
    use tw_agent::server::{AuthLimiter, Context, serve};
    use tw_agent::update::{FileUpdater, old_path};
    use tw_agent::win::{WinDesktop, single_instance};
    use tw_transport::HvSockListener;

    let log = log();
    if let Err(message) = guard::check() {
        log.error(&message);
        eprintln!("{message}");
        return ExitCode::from(1);
    }
    let Some(_instance) = single_instance("Global\\TerminalWindowsAgent") else {
        log.info("Another agent is already running; exiting");
        return ExitCode::SUCCESS;
    };
    let config = match config::load(&data_dir().join("agent.json")) {
        Ok(config) => config,
        Err(error) => {
            log.error(&format!("Cannot load the configuration: {error}"));
            return ExitCode::from(1);
        }
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("twagent.exe"));
    let _ = std::fs::remove_file(old_path(&exe));
    let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            log.error(&format!("Cannot start the runtime: {error}"));
            return ExitCode::from(1);
        }
    };
    log.info(&format!("Agent {VERSION} (build {BUILD}) starting, channel {:?}", config.channel));
    let (channel, service_id) = (config.channel, config.service_id);
    let sink = log.clone();
    let ctx = Arc::new(Context {
        desktop: Arc::new(WinDesktop),
        secret: config.secret,
        version: VERSION.into(),
        build: BUILD,
        updater: Arc::new(FileUpdater::new(exe, BUILD)),
        logs: log.clone(),
        limiter: AuthLimiter::default(),
        events: Box::new(move |message| sink.info(message)),
    });
    match channel {
        Channel::HvSock => {
            let listener = match HvSockListener::bind(service_id) {
                Ok(listener) => Arc::new(listener),
                Err(error) => {
                    log.error(&format!("Cannot listen on the Hyper-V socket: {error}"));
                    return ExitCode::from(1);
                }
            };
            runtime.block_on(serve(
                move || {
                    let listener = listener.clone();
                    async move { listener.accept().await }
                },
                ctx,
            ));
        }
        Channel::Tcp(port) => {
            let listener = match runtime.block_on(tokio::net::TcpListener::bind(("0.0.0.0", port))) {
                Ok(listener) => Arc::new(listener),
                Err(error) => {
                    log.error(&format!("Cannot listen on TCP port {port}: {error}"));
                    return ExitCode::from(1);
                }
            };
            runtime.block_on(serve(
                move || {
                    let listener = listener.clone();
                    async move {
                        let (stream, _) = listener.accept().await?;
                        stream.set_nodelay(true)?;
                        Ok(Box::new(stream) as tw_transport::BoxStream)
                    }
                },
                ctx,
            ));
        }
    }
    ExitCode::SUCCESS
}

#[cfg(windows)]
fn supervise() -> ExitCode {
    use tw_agent::supervise::Supervisor;
    use tw_agent::win::single_instance;

    let log = log();
    if let Err(message) = guard::check() {
        log.error(&message);
        return ExitCode::from(1);
    }
    let Some(_instance) = single_instance("Global\\TerminalWindowsSupervisor") else {
        return ExitCode::SUCCESS;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            log.error(&format!("Cannot find the agent executable: {error}"));
            return ExitCode::from(1);
        }
    };
    let sink = log.clone();
    let supervisor = Supervisor {
        exe,
        args: vec!["run".into()],
        max_runs: None,
        on_event: Box::new(move |message| sink.info(&format!("supervisor: {message}"))),
    };
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => {
            runtime.block_on(supervisor.run());
            ExitCode::SUCCESS
        }
        Err(error) => {
            log.error(&format!("Cannot start the runtime: {error}"));
            ExitCode::from(1)
        }
    }
}
