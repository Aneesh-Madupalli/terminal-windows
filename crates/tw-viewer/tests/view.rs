//! The view loop against a fake terminal, a fake session and a small fake streaming agent over loopback TCP.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tw_client::{FrameStream, GuestError};
use tw_proto::auth::random_bytes;
use tw_proto::frame::compress;
use tw_proto::{AgentHandshake, Header, Limits};
use tw_term::Protocol;
use tw_term::chrome::Theme;
use tw_transport::{BoxStream, Framed, LoopbackTransport, Target};
use tw_viewer::{Exit, Options, Session, Terminal, run};

const SECRET: [u8; 32] = [6; 32];
const WAIT: Duration = Duration::from_secs(5);

/// Serves connections; each one gets a subscribe reply and then a frame every 15 ms. The first connection
/// is cut after `cut_after` frames when that is set.
async fn agent(cut_after: Option<u32>) -> (SocketAddr, Arc<AtomicU32>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicU32::new(0));
    let counter = connections.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let number = counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut framed = Framed::new(Box::new(stream) as BoxStream);
                let handshake = AgentHandshake::new(&SECRET, "0.1.0", 1, random_bytes());
                if framed.write(&handshake.hello(), &[]).await.is_err() {
                    return;
                }
                let Ok(auth) = framed.read(Limits::PRE_AUTH).await else { return };
                let Ok((confirmation, _)) = handshake.verify_auth(&auth.header) else { return };
                if framed.write(&confirmation, &[]).await.is_err() {
                    return;
                }
                let Ok(request) = framed.read(Limits::AUTHENTICATED).await else { return };
                let Header::Req { id, .. } = request.header else { return };
                let reply = Header::Rep { id, ok: true, values: BTreeMap::new(), error: None };
                if framed.write(&reply, &[]).await.is_err() {
                    return;
                }
                for seq in 1u64.. {
                    if number == 0 && cut_after.is_some_and(|n| seq > u64::from(n)) {
                        return;
                    }
                    let pixels: Vec<u8> = (0..64 * 32).flat_map(|_| [(seq % 200) as u8, 40, 90, 255]).collect();
                    let header = Header::Frame {
                        seq,
                        format: "bgra-lz4".into(),
                        width: 64,
                        height: 32,
                        bytes_per_row: 256,
                        screen_width: 1280,
                        screen_height: 640,
                        dpi: 96,
                    };
                    if framed.write(&header, &compress(&pixels)).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(15)).await;
                }
            });
        }
    });
    (address, connections)
}

type Call = (String, BTreeMap<String, String>);

struct FakeSession {
    address: Option<SocketAddr>,
    calls: Mutex<Vec<Call>>,
    ctrl_alt_del: AtomicU32,
    console: AtomicU32,
}

impl FakeSession {
    fn new(address: Option<SocketAddr>) -> Arc<Self> {
        Arc::new(Self {
            address,
            calls: Mutex::new(Vec::new()),
            ctrl_alt_del: AtomicU32::new(0),
            console: AtomicU32::new(0),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Session for FakeSession {
    async fn open_stream(&self) -> Result<FrameStream, GuestError> {
        let Some(address) = self.address else { return Err(GuestError::NotCreated("default".into())) };
        Ok(FrameStream::connect(&LoopbackTransport, &Target::Loopback(address), &SECRET, WAIT).await?)
    }

    async fn call(&self, action: &str, args: BTreeMap<String, String>) -> Result<(), GuestError> {
        self.calls.lock().unwrap().push((action.to_string(), args));
        Ok(())
    }

    async fn ctrl_alt_del(&self) -> Result<(), GuestError> {
        self.ctrl_alt_del.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn open_console(&self) -> Result<(), GuestError> {
        self.console.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn explain(&self, error: &GuestError) -> String {
        error.to_string()
    }
}

struct FakeTerminal {
    output: Mutex<String>,
}

impl FakeTerminal {
    fn new() -> Arc<Self> {
        Arc::new(Self { output: Mutex::new(String::new()) })
    }

    fn output(&self) -> String {
        self.output.lock().unwrap().clone()
    }
}

impl Terminal for FakeTerminal {
    fn size(&self) -> (usize, usize) {
        (40, 11)
    }

    fn write(&self, text: &str) {
        self.output.lock().unwrap().push_str(text);
    }

    fn write_and_wait(&self, text: &str) {
        self.write(text);
    }
}

fn options() -> Options {
    Options {
        name: "default".into(),
        protocol: Some(Protocol::Blocks),
        cell: Some((8.0, 16.0)),
        theme: Theme::new(false),
        probe_wait: Duration::from_millis(20),
    }
}

struct Run {
    input: UnboundedSender<Vec<u8>>,
    term: Arc<FakeTerminal>,
    session: Arc<FakeSession>,
    task: tokio::task::JoinHandle<Exit>,
}

fn start(address: Option<SocketAddr>) -> Run {
    let (input, receiver) = unbounded_channel();
    let term = FakeTerminal::new();
    let session = FakeSession::new(address);
    let task = tokio::spawn(run(session.clone(), term.clone(), receiver, options()));
    Run { input, term, session, task }
}

async fn until(what: &str, condition: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !condition() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

impl Run {
    async fn finish(self) -> Exit {
        tokio::time::timeout(WAIT, self.task).await.expect("the view ends").unwrap()
    }
}

#[tokio::test]
async fn frames_are_drawn_and_ctrl_right_bracket_quits() {
    let (address, _) = agent(None).await;
    let run = start(Some(address));
    until("the first frame", || run.term.output().contains('\u{2580}')).await;
    until("the live status", || run.term.output().contains("live")).await;
    run.input.send(vec![0x1d]).unwrap();
    let term = run.term.clone();
    assert_eq!(run.finish().await, Exit::Quit);
    let output = term.output();
    assert!(output.starts_with("\x1b[?1049h"), "alternate screen is entered first");
    assert!(output.ends_with("\x1b[?1049l"), "and restored last");
    assert!(output.contains("\x1b[?25h"), "the cursor comes back");
}

#[tokio::test]
async fn typed_text_and_keys_reach_the_guest() {
    let (address, _) = agent(None).await;
    let run = start(Some(address));
    until("the first frame", || run.term.output().contains('\u{2580}')).await;
    run.input.send(b"hi".to_vec()).unwrap();
    until("the typed text", || run.session.calls().iter().any(|(a, _)| a == "type")).await;
    run.input.send(vec![0x03]).unwrap();
    until("the key", || run.session.calls().iter().any(|(a, _)| a == "key")).await;
    run.input.send(vec![0x1d]).unwrap();
    let session = run.session.clone();
    run.finish().await;
    let calls = session.calls();
    assert_eq!(calls[0].0, "type");
    assert_eq!(calls[0].1["text"], "hi");
    assert_eq!(calls[1].1["key"], "ctrl+c");
}

#[tokio::test]
async fn a_mouse_click_on_the_desktop_is_sent_in_guest_pixels() {
    let (address, _) = agent(None).await;
    let run = start(Some(address));
    until("the first frame", || run.term.output().contains('\u{2580}')).await;
    run.input.send(b"\x1b[<0;20;5M\x1b[<0;20;5m".to_vec()).unwrap();
    until("the click", || run.session.calls().iter().any(|(a, _)| a == "click")).await;
    run.input.send(vec![0x1d]).unwrap();
    let session = run.session.clone();
    run.finish().await;
    let click = session.calls().into_iter().find(|(a, _)| a == "click").unwrap().1;
    let x: u32 = click["x"].parse().unwrap();
    let y: u32 = click["y"].parse().unwrap();
    assert!(x < 1280 && y < 640 && x > 300, "({x}, {y})");
}

#[tokio::test]
async fn the_menu_sends_ctrl_alt_del_and_opens_the_console() {
    let (address, _) = agent(None).await;
    let run = start(Some(address));
    until("the first frame", || run.term.output().contains('\u{2580}')).await;
    run.input.send(vec![0x07]).unwrap();
    until("the menu", || run.term.output().contains("Ctrl+Alt+Del")).await;
    run.input.send(b"d".to_vec()).unwrap();
    until("ctrl+alt+del", || run.session.ctrl_alt_del.load(Ordering::SeqCst) == 1).await;
    run.input.send(vec![0x07]).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    run.input.send(b"o".to_vec()).unwrap();
    until("the console", || run.session.console.load(Ordering::SeqCst) == 1).await;
    assert!(run.session.calls().is_empty(), "menu keys are not typed into the guest");
    run.input.send(vec![0x07]).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    run.input.send(b"q".to_vec()).unwrap();
    assert_eq!(run.finish().await, Exit::Quit);
}

#[tokio::test]
async fn a_dropped_connection_is_reestablished() {
    let (address, connections) = agent(Some(3)).await;
    let run = start(Some(address));
    until("a second connection", || connections.load(Ordering::SeqCst) >= 2).await;
    until("reconnecting to be shown", || run.term.output().contains("reconnecting")).await;
    let before = run.term.output().len();
    until("frames again", || run.term.output().len() > before + 200).await;
    run.input.send(vec![0x1d]).unwrap();
    run.finish().await;
}

#[tokio::test]
async fn a_missing_machine_ends_the_view() {
    let run = start(None);
    let term = run.term.clone();
    assert_eq!(run.finish().await, Exit::Gone("default".into()));
    assert!(term.output().ends_with("\x1b[?1049l"));
}

#[tokio::test]
async fn closed_input_ends_the_view() {
    let (address, _) = agent(None).await;
    let run = start(Some(address));
    until("the first frame", || run.term.output().contains('\u{2580}')).await;
    drop(run.input);
    assert_eq!(tokio::time::timeout(WAIT, run.task).await.unwrap().unwrap(), Exit::InputClosed);
}
