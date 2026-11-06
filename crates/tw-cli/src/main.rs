//! `terminal-windows`: wires the real Hyper-V, PowerShell and Hyper-V sockets into the commands.

use std::process::ExitCode;

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("terminal-windows runs only on Windows");
    ExitCode::from(1)
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> ExitCode {
    windows::main().await
}

#[cfg(windows)]
mod windows {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::time::Duration;

    use sha2::{Digest, Sha256};
    use tw_cli::commands;
    use tw_cli::env::{Env, GuestFactory, PsMedia, SharedGuest, SystemCredentials, Timing};
    use tw_client::{Guest, LiveGuest};
    use tw_core::args::Arguments;
    use tw_core::{Error, Result, Store};
    use tw_hv::{HyperV, Hypervisor, VirtualBox, VmHandle};
    use tw_mcp::McpServer;
    use tw_transport::AutoTransport;
    use tw_viewer::{Exit, LiveSession, Options, WindowsTerminal};
    use uuid::Uuid;

    /// Keeps Hyper-V names unique when several Windows users share one host.
    fn user_tag() -> String {
        let identity = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        )
        .to_lowercase();
        Sha256::digest(identity.as_bytes())[..4].iter().map(|b| format!("{b:02x}")).collect()
    }

    fn agent_exe() -> PathBuf {
        if let Some(path) = std::env::var_os("TERMINAL_WINDOWS_AGENT") {
            return PathBuf::from(path);
        }
        std::env::current_exe()
            .map(|exe| exe.with_file_name("twagent.exe"))
            .unwrap_or_else(|_| PathBuf::from("twagent.exe"))
    }

    fn build_env() -> Result<Env> {
        let store = Store::from_env()?;
        let hypervisors: Vec<Arc<dyn Hypervisor>> =
            vec![Arc::new(HyperV::new(tw_proto::SERVICE_GUID)), Arc::new(VirtualBox::new())];
        let service = Uuid::parse_str(tw_proto::SERVICE_GUID).expect("the constant is a valid GUID");
        let (factory_store, factory_hypervisors) = (store.clone(), hypervisors.clone());
        let guest: GuestFactory = Arc::new(move |name| {
            let hv = backend_of(&factory_store, &factory_hypervisors, name);
            Arc::new(LiveGuest::new(factory_store.clone(), name, hv, Arc::new(AutoTransport), service))
                as Arc<dyn Guest>
        });
        Ok(Env {
            store,
            hypervisors,
            media: Arc::new(PsMedia::new()),
            guest,
            credentials: Arc::new(SystemCredentials),
            agent_exe: agent_exe(),
            user_tag: user_tag(),
            timing: Timing::default(),
            progress: Arc::new(|message| eprintln!("{message}")),
        })
    }

    /// The backend a machine was created with; the first one when the machine does not exist yet.
    fn backend_of(store: &Store, hypervisors: &[Arc<dyn Hypervisor>], name: &str) -> Arc<dyn Hypervisor> {
        let wanted = store.load(name).map(|m| m.backend).unwrap_or_default();
        hypervisors.iter().find(|hv| hv.id() == wanted).unwrap_or(&hypervisors[0]).clone()
    }

    async fn setup_host(rest: &[String]) -> Result<String> {
        let args = Arguments::parse(rest, &["enable-hyper-v"], &[])?;
        let report = HyperV::new(tw_proto::SERVICE_GUID).setup_host(args.flag("enable-hyper-v")).await?;
        let mut text = String::new();
        if report.registered {
            text.push_str("Registered the guest control channel with Hyper-V.\n");
        }
        if report.added_to_group {
            text.push_str("Added your account to Hyper-V Administrators. Sign out and in for it to apply.\n");
        }
        if report.restart_required {
            text.push_str("Restart Windows to finish enabling Hyper-V.\n");
        }
        if text.is_empty() {
            text.push_str("The host is already set up.\n");
        }
        Ok(text)
    }

    async fn mcp(env: &Env, rest: &[String]) -> Result<String> {
        let args = Arguments::parse(rest, &[], &[])?;
        let name = args.machine_name()?;
        let server = McpServer::new(SharedGuest((env.guest)(&name)), "terminal-windows", tw_agent::VERSION);
        server
            .serve(tokio::io::BufReader::new(tokio::io::stdin()), tokio::io::stdout())
            .await
            .map_err(|e| Error::Other(format!("The MCP session ended: {e}")))?;
        Ok(String::new())
    }

    fn parse_cell(text: &str) -> Result<(f64, f64)> {
        let parsed = text.split_once('x').and_then(|(w, h)| Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?)));
        match parsed {
            Some((w, h)) if (4.0..=64.0).contains(&w) && (8.0..=128.0).contains(&h) => Ok((w, h)),
            _ => Err(Error::Usage("--cell must look like 10x20 (the terminal cell size in pixels)".into())),
        }
    }

    async fn view(env: &Env, rest: &[String]) -> Result<String> {
        let args = Arguments::parse(rest, &[], &["protocol", "cell"])?;
        let name = args.machine_name()?;
        let machine = env.store.load(&name)?;
        let protocol =
            args.option("protocol").map(str::parse::<tw_term::Protocol>).transpose().map_err(Error::Usage)?;
        let cell = match args.option("cell").map(str::to_string).or_else(|| std::env::var("TERMINAL_WINDOWS_CELL").ok())
        {
            Some(text) => Some(parse_cell(&text)?),
            None => None,
        };
        let service = Uuid::parse_str(tw_proto::SERVICE_GUID).expect("the constant is a valid GUID");
        let hv = env.hv_for(&machine)?;
        let guest = LiveGuest::new(env.store.clone(), name.clone(), hv.clone(), Arc::new(AutoTransport), service);
        let session = Arc::new(LiveSession::new(guest, hv, VmHandle::new(&machine.hv_name), name.clone()));
        let (terminal, input) = WindowsTerminal::open()?;
        let options = Options {
            name: name.clone(),
            protocol,
            cell,
            theme: tw_term::chrome::Theme::from_env(),
            probe_wait: Duration::from_millis(300),
        };
        let terminal = Arc::new(terminal);
        let exit = tw_viewer::run(session, terminal.clone(), input, options).await;
        drop(terminal);
        match exit {
            Exit::Gone(missing) => Err(Error::NoSuchVm(missing)),
            Exit::Quit | Exit::InputClosed => Ok(String::new()),
        }
    }

    async fn dispatch(args: &[String]) -> Result<String> {
        match args.first().map(String::as_str) {
            Some("view") => view(&build_env()?, &args[1..]).await,
            Some("setup-host") => setup_host(&args[1..]).await,
            Some("mcp") => mcp(&build_env()?, &args[1..]).await,
            _ => commands::run(&build_env()?, args).await,
        }
    }

    pub async fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match dispatch(&args).await {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("Error: {error}");
                ExitCode::from(error.exit_code() as u8)
            }
        }
    }
}
