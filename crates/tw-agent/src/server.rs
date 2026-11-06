//! The agent's connection handler: authentication, requests, and the live frame stream. It is independent of
//! the operating system; everything platform-specific sits behind `Desktop`, `Updater` and `LogSource`.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::time::Instant;
use tw_proto::auth::random_bytes;
use tw_proto::frame::compress;
use tw_proto::{Action, AgentHandshake, ErrorCode, Header, Limits, ProtoError, parse_action};
use tw_transport::{BoxStream, Framed};
use uuid::Uuid;

use crate::desktop::{Bitmap, Desktop, DesktopError, Session};
use crate::input::{execute, plan};
use crate::log::FileLog;
use crate::scale::{downscale, fingerprint, fit, to_png};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
/// A subscriber that acknowledges nothing for this long is dropped.
const ACK_SILENCE: Duration = Duration::from_secs(15);
const MAX_UNACKED: u64 = 2;
const MAX_CONNECTIONS: usize = 8;
/// After this many unchanged captures in a row the poll rate drops, which saves CPU on a still desktop.
const IDLE_AFTER: u32 = 15;
const IDLE_INTERVAL: Duration = Duration::from_millis(66);
const FAILURE_BACKOFF: Duration = Duration::from_millis(500);

pub trait Updater: Send + Sync {
    /// Verifies and installs a new agent binary.
    fn apply(&self, sha256: &str, build: u32, payload: &[u8]) -> Result<(), String>;
    /// Called after the reply was sent; the real implementation ends the process.
    fn restart(&self);
}

pub trait LogSource: Send + Sync {
    fn tail(&self, lines: usize) -> String;
}

impl LogSource for FileLog {
    fn tail(&self, lines: usize) -> String {
        FileLog::tail(self, lines)
    }
}

/// Slows down guessing: every failed handshake costs a delay, and more than three in a minute cost more.
#[derive(Default)]
pub struct AuthLimiter {
    failures: Mutex<VecDeque<Instant>>,
}

impl AuthLimiter {
    pub fn record_failure(&self, now: Instant) -> Duration {
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        failures.push_back(now);
        while failures.front().is_some_and(|first| now.duration_since(*first) > Duration::from_secs(60)) {
            failures.pop_front();
        }
        if failures.len() > 3 { Duration::from_secs(2) } else { Duration::from_millis(250) }
    }
}

pub struct Context {
    pub desktop: Arc<dyn Desktop>,
    pub secret: Vec<u8>,
    pub version: String,
    pub build: u32,
    pub updater: Arc<dyn Updater>,
    pub logs: Arc<dyn LogSource>,
    pub limiter: AuthLimiter,
    pub events: Box<dyn Fn(&str) + Send + Sync>,
}

impl Context {
    fn event(&self, message: &str) {
        (self.events)(message);
    }
}

enum Effect {
    None,
    Subscribe { max_width: u32, max_height: u32, fps: u32 },
    Unsubscribe,
    Restart,
}

struct Outcome {
    header: Header,
    payload: Vec<u8>,
    effect: Effect,
}

fn values(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

fn success(id: Uuid, pairs: &[(&str, String)], payload: Vec<u8>, effect: Effect) -> Outcome {
    Outcome { header: Header::Rep { id, ok: true, values: values(pairs), error: None }, payload, effect }
}

fn failure(id: Uuid, code: ErrorCode, message: impl Into<String>) -> Outcome {
    let values = values(&[("code", code.as_str().to_string())]);
    Outcome {
        header: Header::Rep { id, ok: false, values, error: Some(message.into()) },
        payload: Vec::new(),
        effect: Effect::None,
    }
}

fn desktop_failure(id: Uuid, error: DesktopError) -> Outcome {
    failure(id, error.code, error.message)
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, DesktopError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| DesktopError::new(ErrorCode::Internal, "The agent hit an internal error"))
}

async fn handle(
    ctx: &Arc<Context>,
    id: Uuid,
    action: &str,
    args: &BTreeMap<String, String>,
    payload: &[u8],
) -> Outcome {
    let screen = ctx.desktop.screen().ok().map(|s| (s.width, s.height));
    let parsed = match parse_action(action, args, screen) {
        Ok(parsed) => parsed,
        Err(error) => {
            let message = error.to_string();
            let code =
                if message.contains("Unknown action") { ErrorCode::UnknownAction } else { ErrorCode::BadRequest };
            return failure(id, code, message);
        }
    };
    let none = Effect::None;
    match parsed {
        Action::Ping => success(id, &[("ok", "true".into())], Vec::new(), none),
        Action::Status => {
            let desktop = ctx.desktop.clone();
            let Ok((session, screen, os)) = blocking(move || (desktop.session(), desktop.screen(), desktop.os())).await
            else {
                return failure(id, ErrorCode::Internal, "The agent hit an internal error");
            };
            let mut pairs = vec![
                ("session", session.as_str().to_string()),
                ("version", ctx.version.clone()),
                ("build", ctx.build.to_string()),
                ("os", os),
            ];
            if let Ok(screen) = screen {
                pairs.push(("width", screen.width.to_string()));
                pairs.push(("height", screen.height.to_string()));
                pairs.push(("dpi", screen.dpi.to_string()));
            }
            success(id, &pairs, Vec::new(), none)
        }
        Action::Screenshot => {
            let desktop = ctx.desktop.clone();
            let captured = blocking(move || -> Result<(u32, u32, Vec<u8>), DesktopError> {
                if let Some(refusal) = desktop.session().refusal() {
                    return Err(refusal);
                }
                let bitmap = desktop.capture()?;
                let png = to_png(&bitmap).map_err(|e| DesktopError::new(ErrorCode::CaptureFailed, e))?;
                Ok((bitmap.width, bitmap.height, png))
            })
            .await;
            match captured {
                Ok(Ok((width, height, png))) => success(
                    id,
                    &[("image", "screenshot.png".into()), ("width", width.to_string()), ("height", height.to_string())],
                    png,
                    none,
                ),
                Ok(Err(error)) | Err(error) => desktop_failure(id, error),
            }
        }
        Action::Subscribe { max_width, max_height, fps } => {
            let (width, height) = match ctx.desktop.screen() {
                Ok(screen) => fit(screen.width, screen.height, max_width, max_height),
                Err(_) => (0, 0),
            };
            success(
                id,
                &[("subscribed", "true".into()), ("width", width.to_string()), ("height", height.to_string())],
                Vec::new(),
                Effect::Subscribe { max_width, max_height, fps },
            )
        }
        Action::Unsubscribe => success(id, &[("ok", "true".into())], Vec::new(), Effect::Unsubscribe),
        Action::Click { .. }
        | Action::Move { .. }
        | Action::Drag { .. }
        | Action::Scroll { .. }
        | Action::Type { .. }
        | Action::Key { .. } => {
            let steps = match plan(&parsed) {
                Ok(steps) => steps,
                Err(error) => return desktop_failure(id, error),
            };
            let desktop = ctx.desktop.clone();
            let result = blocking(move || -> Result<(), DesktopError> {
                if let Some(refusal) = desktop.session().refusal() {
                    return Err(refusal);
                }
                execute(&*desktop, &steps)
            })
            .await;
            match result {
                Ok(Ok(())) => success(id, &[("ok", "true".into())], Vec::new(), none),
                Ok(Err(error)) | Err(error) => desktop_failure(id, error),
            }
        }
        Action::Update { sha256, build, size } => {
            if payload.len() as u64 != size {
                return failure(id, ErrorCode::BadRequest, "The update payload does not match its declared size");
            }
            let updater = ctx.updater.clone();
            let payload = payload.to_vec();
            match blocking(move || updater.apply(&sha256, build, &payload)).await {
                Ok(Ok(())) => {
                    ctx.event(&format!("Installed agent build {build}; restarting"));
                    success(
                        id,
                        &[("staged", "true".into()), ("restarting", "true".into())],
                        Vec::new(),
                        Effect::Restart,
                    )
                }
                Ok(Err(message)) => {
                    ctx.event(&format!("Update refused: {message}"));
                    failure(id, ErrorCode::BadRequest, message)
                }
                Err(error) => desktop_failure(id, error),
            }
        }
        Action::Logs { lines } => {
            let text = ctx.logs.tail(lines as usize);
            let count = text.lines().count();
            success(id, &[("lines", count.to_string())], text.into_bytes(), none)
        }
    }
}

struct Subscription {
    max_width: u32,
    max_height: u32,
    interval: Duration,
    next_due: Instant,
    seq: u64,
    acked: u64,
    last_ack: Instant,
    last_print: Option<u64>,
    unchanged: u32,
}

struct Captured {
    print: u64,
    header: Header,
    payload: Vec<u8>,
}

fn frame_from(bitmap: &Bitmap, screen: (u32, u32, u32), max: (u32, u32), seq: u64) -> (Header, Vec<u8>) {
    let (width, height) = fit(bitmap.width, bitmap.height, max.0, max.1);
    let scaled = downscale(bitmap, width, height);
    let payload = compress(&scaled.data);
    let header = Header::Frame {
        seq,
        format: "bgra-lz4".into(),
        width,
        height,
        bytes_per_row: width * 4,
        screen_width: screen.0,
        screen_height: screen.1,
        dpi: screen.2,
    };
    (header, payload)
}

async fn write_with_timeout(framed: &mut Framed, header: &Header, payload: &[u8]) -> Result<(), ProtoError> {
    tokio::time::timeout(WRITE_TIMEOUT, framed.write(header, payload))
        .await
        .map_err(|_| ProtoError::Io(io::ErrorKind::TimedOut.into()))?
}

/// Captures and sends one frame if the screen changed and the viewer has room. Returns false when the
/// subscription should end.
async fn push_frame(
    framed: &mut Framed,
    ctx: &Arc<Context>,
    subscription: &mut Subscription,
) -> Result<bool, ProtoError> {
    let now = Instant::now();
    subscription.next_due = now + subscription.interval;
    if subscription.seq - subscription.acked >= MAX_UNACKED {
        return Ok(now.duration_since(subscription.last_ack) <= ACK_SILENCE);
    }
    let desktop = ctx.desktop.clone();
    let max = (subscription.max_width, subscription.max_height);
    let last_print = subscription.last_print;
    let seq = subscription.seq + 1;
    let captured = blocking(move || -> Result<Option<Captured>, DesktopError> {
        if desktop.session() != Session::Ready {
            return Ok(None);
        }
        let screen = desktop.screen()?;
        let bitmap = desktop.capture()?;
        let print = fingerprint(&bitmap);
        if last_print == Some(print) {
            return Ok(None);
        }
        let (header, payload) = frame_from(&bitmap, (screen.width, screen.height, screen.dpi), max, seq);
        Ok(Some(Captured { print, header, payload }))
    })
    .await;
    match captured {
        Ok(Ok(Some(frame))) => {
            write_with_timeout(framed, &frame.header, &frame.payload).await?;
            if subscription.seq == subscription.acked {
                subscription.last_ack = Instant::now();
            }
            subscription.seq = seq;
            subscription.last_print = Some(frame.print);
            subscription.unchanged = 0;
        }
        Ok(Ok(None)) => {
            subscription.unchanged = subscription.unchanged.saturating_add(1);
            if subscription.unchanged >= IDLE_AFTER {
                subscription.next_due = now + subscription.interval.max(IDLE_INTERVAL);
            }
        }
        Ok(Err(_)) | Err(_) => subscription.next_due = now + FAILURE_BACKOFF,
    }
    Ok(true)
}

async fn handshake(framed: &mut Framed, ctx: &Context) -> Result<(), ProtoError> {
    let exchange = async {
        let handshake = AgentHandshake::new(&ctx.secret, &ctx.version, ctx.build, random_bytes());
        framed.write(&handshake.hello(), &[]).await?;
        let auth = framed.read(Limits::PRE_AUTH).await?;
        let (confirmation, _) = handshake.verify_auth(&auth.header)?;
        framed.write(&confirmation, &[]).await
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, exchange)
        .await
        .unwrap_or_else(|_| Err(ProtoError::Io(io::ErrorKind::TimedOut.into())))
}

/// Serves one authenticated connection until it closes.
pub async fn serve_connection(stream: BoxStream, ctx: Arc<Context>) -> Result<(), ProtoError> {
    let mut framed = Framed::new(stream);
    if let Err(error) = handshake(&mut framed, &ctx).await {
        let delay = ctx.limiter.record_failure(Instant::now());
        ctx.event(&format!("Handshake failed: {error}"));
        tokio::time::sleep(delay).await;
        return Err(error);
    }
    ctx.event("Host connected");
    let mut subscription: Option<Subscription> = None;
    loop {
        let due_at = subscription.as_ref().map(|s| s.next_due);
        let due = async move {
            match due_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            message = framed.read(Limits::AUTHENTICATED) => {
                let message = message?;
                match message.header {
                    Header::Req { id, action, args, .. } => {
                        let outcome = handle(&ctx, id, &action, &args, &message.payload).await;
                        write_with_timeout(&mut framed, &outcome.header, &outcome.payload).await?;
                        match outcome.effect {
                            Effect::None => {}
                            Effect::Subscribe { max_width, max_height, fps } => {
                                let now = Instant::now();
                                let previous = subscription.take();
                                subscription = Some(Subscription {
                                    max_width,
                                    max_height,
                                    interval: Duration::from_secs(1) / fps,
                                    next_due: now,
                                    seq: previous.as_ref().map_or(0, |p| p.seq),
                                    acked: previous.as_ref().map_or(0, |p| p.seq),
                                    last_ack: now,
                                    last_print: None,
                                    unchanged: 0,
                                });
                            }
                            Effect::Unsubscribe => subscription = None,
                            Effect::Restart => {
                                ctx.updater.restart();
                                return Ok(());
                            }
                        }
                    }
                    Header::Ack { seq } => {
                        if let Some(active) = subscription.as_mut() {
                            active.acked = active.acked.max(seq.min(active.seq));
                            active.last_ack = Instant::now();
                        }
                    }
                    _ => return Err(ProtoError::Unexpected("req")),
                }
            }
            () = due => {
                if let Some(active) = subscription.as_mut() {
                    if !push_frame(&mut framed, &ctx, active).await? {
                        ctx.event("Viewer stopped acknowledging frames; ending the stream");
                        subscription = None;
                    }
                }
            }
        }
    }
}

/// Accepts connections forever and serves each on its own task, up to a small limit.
pub async fn serve<A, F>(mut accept: A, ctx: Arc<Context>)
where
    A: FnMut() -> F,
    F: Future<Output = io::Result<BoxStream>>,
{
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        match accept().await {
            Ok(stream) => {
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    ctx.event("Too many connections; refused one");
                    continue;
                };
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let result = serve_connection(stream, ctx.clone()).await;
                    if let Err(error) = result {
                        if !matches!(&error, ProtoError::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof) {
                            ctx.event(&format!("Connection ended: {error}"));
                        }
                    }
                    drop(permit);
                });
            }
            Err(error) => {
                ctx.event(&format!("Accept failed: {error}"));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::net::{TcpListener, TcpStream};
    use tw_client::{ClientError, GuestClient};
    use tw_proto::HostHandshake;
    use tw_proto::frame::decompress;
    use tw_transport::{LoopbackTransport, Target};

    use super::*;
    use crate::desktop::fake::FakeDesktop;

    const SECRET: [u8; 32] = [5; 32];

    struct FakeUpdater {
        restarted: AtomicBool,
        applied: Mutex<Vec<(String, u32, usize)>>,
        fail: bool,
    }

    impl Updater for FakeUpdater {
        fn apply(&self, sha256: &str, build: u32, payload: &[u8]) -> Result<(), String> {
            if self.fail {
                return Err("does not match".into());
            }
            self.applied.lock().unwrap().push((sha256.to_string(), build, payload.len()));
            Ok(())
        }

        fn restart(&self) {
            self.restarted.store(true, Ordering::SeqCst);
        }
    }

    struct Logs;

    impl LogSource for Logs {
        fn tail(&self, lines: usize) -> String {
            (0..lines.min(3)).map(|i| format!("log {i}")).collect::<Vec<_>>().join("\n")
        }
    }

    struct Fixture {
        address: SocketAddr,
        desktop: Arc<FakeDesktop>,
        updater: Arc<FakeUpdater>,
        events: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn fixture(width: u32, height: u32, fail_update: bool) -> Fixture {
        let desktop = Arc::new(FakeDesktop::new(width, height));
        let updater = Arc::new(FakeUpdater {
            restarted: AtomicBool::new(false),
            applied: Mutex::new(Vec::new()),
            fail: fail_update,
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let ctx = Arc::new(Context {
            desktop: desktop.clone(),
            secret: SECRET.to_vec(),
            version: "0.1.0".into(),
            build: 3,
            updater: updater.clone(),
            logs: Arc::new(Logs),
            limiter: AuthLimiter::default(),
            events: Box::new(move |m| sink.lock().unwrap().push(m.to_string())),
        });
        let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(serve(
            move || {
                let listener = listener.clone();
                async move { listener.accept().await.map(|(s, _)| Box::new(s) as BoxStream) }
            },
            ctx,
        ));
        Fixture { address, desktop, updater, events, task }
    }

    async fn client(address: SocketAddr) -> GuestClient {
        GuestClient::connect(&LoopbackTransport, &Target::Loopback(address), &SECRET, Duration::from_secs(5))
            .await
            .unwrap()
    }

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    const T: Duration = Duration::from_secs(5);

    async fn remote_code(client: &GuestClient, action: &str, pairs: &[(&str, &str)]) -> String {
        match client.call(action, args(pairs), T).await.unwrap_err() {
            ClientError::Remote { code, .. } => code.unwrap_or_default(),
            other => panic!("expected a remote error, got {other}"),
        }
    }

    #[tokio::test]
    async fn status_ping_and_unknown_actions() {
        let f = fixture(640, 400, false).await;
        let c = client(f.address).await;
        assert_eq!(c.agent.version, "0.1.0");
        assert_eq!(c.agent.build, 3);
        let status = c.call("status", BTreeMap::new(), T).await.unwrap().values;
        assert_eq!(status["session"], "ready");
        assert_eq!((status["width"].as_str(), status["height"].as_str(), status["dpi"].as_str()), ("640", "400", "96"));
        assert_eq!(status["os"], "Fake OS");
        assert_eq!(c.call("ping", BTreeMap::new(), T).await.unwrap().values["ok"], "true");
        assert_eq!(remote_code(&c, "teleport", &[]).await, "unknown-action");
        assert_eq!(remote_code(&c, "ping", &[("x", "1")]).await, "bad-request");
    }

    #[tokio::test]
    async fn screenshot_returns_a_png_of_the_screen() {
        let f = fixture(64, 48, false).await;
        let reply = client(f.address).await.call("screenshot", BTreeMap::new(), T).await.unwrap();
        assert_eq!((reply.values["width"].as_str(), reply.values["height"].as_str()), ("64", "48"));
        let mut decoder = png::Decoder::new(std::io::Cursor::new(reply.payload)).read_info().unwrap();
        let mut pixels = vec![0u8; decoder.output_buffer_size().unwrap()];
        let info = decoder.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (64, 48));
        // The fake paints blue = column, green = 7, red = row.
        assert_eq!(&pixels[3..6], &[0, 7, 1], "pixel 1: red 0, green 7, blue 1");
    }

    #[tokio::test]
    async fn input_actions_reach_the_desktop_and_are_range_checked() {
        let f = fixture(640, 400, false).await;
        let c = client(f.address).await;
        c.call("click", args(&[("x", "10"), ("y", "20"), ("count", "2")]), T).await.unwrap();
        assert_eq!(f.desktop.sent.lock().unwrap().len(), 5);
        c.call("type", args(&[("text", "hi")]), T).await.unwrap();
        c.call("key", args(&[("key", "ctrl+c")]), T).await.unwrap();
        c.call("scroll", args(&[("dy", "120")]), T).await.unwrap();
        assert_eq!(remote_code(&c, "click", &[("x", "640"), ("y", "0")]).await, "bad-request");
        assert_eq!(remote_code(&c, "key", &[("key", "ctrl+alt+del")]).await, "bad-request");
        *f.desktop.fail_send.lock().unwrap() = true;
        assert_eq!(remote_code(&c, "move", &[("x", "1"), ("y", "1")]).await, "input-failed");
    }

    #[tokio::test]
    async fn an_unusable_session_refuses_input_and_screenshots_but_still_reports_status() {
        let f = fixture(64, 48, false).await;
        let c = client(f.address).await;
        for (session, code) in [
            (Session::Locked, "locked"),
            (Session::SecureDesktop, "secure-desktop"),
            (Session::NoSession, "no-session"),
        ] {
            *f.desktop.session.lock().unwrap() = session;
            assert_eq!(remote_code(&c, "click", &[("x", "1"), ("y", "1")]).await, code);
            assert_eq!(remote_code(&c, "screenshot", &[]).await, code);
            assert_eq!(c.call("status", BTreeMap::new(), T).await.unwrap().values["session"], session.as_str());
        }
        assert!(f.desktop.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn logs_come_back_as_text() {
        let f = fixture(64, 48, false).await;
        let reply = client(f.address).await.call("logs", args(&[("lines", "2")]), T).await.unwrap();
        assert_eq!(String::from_utf8(reply.payload).unwrap(), "log 0\nlog 1");
        assert_eq!(reply.values["lines"], "2");
    }

    #[tokio::test]
    async fn update_installs_and_restarts_after_replying() {
        let f = fixture(64, 48, false).await;
        let c = client(f.address).await;
        // The client has no payload-carrying call, so use the raw protocol.
        let mut raw = raw_connect(f.address, &SECRET).await;
        let sha = "a".repeat(64);
        let id = Uuid::new_v4();
        let request = Header::Req {
            id,
            action: "update".into(),
            args: args(&[("sha256", &sha), ("build", "9"), ("size", "5")]),
            deadline_ms: 5000,
        };
        raw.write(&request, b"12345").await.unwrap();
        let reply = raw.read(Limits::AUTHENTICATED).await.unwrap();
        let Header::Rep { ok, values, .. } = reply.header else { panic!("reply expected") };
        assert!(ok);
        assert_eq!((values["staged"].as_str(), values["restarting"].as_str()), ("true", "true"));
        assert_eq!(f.updater.applied.lock().unwrap().as_slice(), [(sha, 9, 5)]);
        assert!(f.updater.restarted.load(Ordering::SeqCst));
        drop(c);
    }

    #[tokio::test]
    async fn update_with_a_wrong_size_or_a_refusing_updater_fails_without_restarting() {
        let f = fixture(64, 48, false).await;
        let mut raw = raw_connect(f.address, &SECRET).await;
        let sha = "b".repeat(64);
        let request = |size: &str| Header::Req {
            id: Uuid::new_v4(),
            action: "update".into(),
            args: args(&[("sha256", &sha), ("build", "9"), ("size", size)]),
            deadline_ms: 5000,
        };
        raw.write(&request("6"), b"12345").await.unwrap();
        let Header::Rep { ok, error, .. } = raw.read(Limits::AUTHENTICATED).await.unwrap().header else { panic!() };
        assert!(!ok && error.unwrap().contains("size"));
        assert!(!f.updater.restarted.load(Ordering::SeqCst));

        let refusing = fixture(64, 48, true).await;
        let mut raw = raw_connect(refusing.address, &SECRET).await;
        raw.write(&request("5"), b"12345").await.unwrap();
        let Header::Rep { ok, error, .. } = raw.read(Limits::AUTHENTICATED).await.unwrap().header else { panic!() };
        assert!(!ok && error.unwrap().contains("does not match"));
        assert!(!refusing.updater.restarted.load(Ordering::SeqCst));
    }

    async fn raw_connect(address: SocketAddr, secret: &[u8]) -> Framed {
        let stream = TcpStream::connect(address).await.unwrap();
        let mut framed = Framed::new(Box::new(stream) as BoxStream);
        let hello = framed.read(Limits::PRE_AUTH).await.unwrap();
        let mut host = HostHandshake::new(secret, random_bytes());
        framed.write(&host.on_hello(&hello.header).unwrap(), &[]).await.unwrap();
        let confirmation = framed.read(Limits::PRE_AUTH).await.unwrap();
        host.on_auth_ok(&confirmation.header).unwrap();
        framed
    }

    #[tokio::test]
    async fn a_wrong_secret_is_rejected_and_logged() {
        let f = fixture(64, 48, false).await;
        let error =
            GuestClient::connect(&LoopbackTransport, &Target::Loopback(f.address), &[9; 32], Duration::from_secs(5))
                .await
                .err()
                .expect("the handshake must fail");
        assert!(
            matches!(error, ClientError::Proto(_) | ClientError::Timeout { .. } | ClientError::Connect(_)),
            "{error}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(f.events.lock().unwrap().iter().any(|e| e.contains("Handshake failed")));
        // The right secret still works afterwards.
        assert_eq!(client(f.address).await.call("ping", BTreeMap::new(), T).await.unwrap().values["ok"], "true");
    }

    #[tokio::test]
    async fn a_peer_that_sends_a_payload_before_authenticating_is_dropped() {
        let f = fixture(64, 48, false).await;
        let stream = TcpStream::connect(f.address).await.unwrap();
        let mut framed = Framed::new(Box::new(stream) as BoxStream);
        framed.read(Limits::PRE_AUTH).await.unwrap();
        // A well-formed request with a payload is not allowed before authentication.
        let request = Header::Req { id: Uuid::new_v4(), action: "ping".into(), args: BTreeMap::new(), deadline_ms: 1 };
        framed.write(&request, b"x").await.unwrap();
        assert!(framed.read(Limits::AUTHENTICATED).await.is_err());
    }

    #[test]
    fn the_limiter_slows_down_after_three_failures_in_a_minute() {
        let limiter = AuthLimiter::default();
        let start = Instant::now();
        let delays: Vec<_> = (0..5).map(|i| limiter.record_failure(start + Duration::from_secs(i))).collect();
        assert_eq!(&delays[..3], &[Duration::from_millis(250); 3]);
        assert_eq!(&delays[3..], &[Duration::from_secs(2); 2]);
        assert_eq!(
            limiter.record_failure(start + Duration::from_secs(200)),
            Duration::from_millis(250),
            "old failures expire"
        );
    }

    async fn next_frame(framed: &mut Framed) -> (Header, Vec<u8>) {
        loop {
            let message =
                tokio::time::timeout(T, framed.read(Limits::AUTHENTICATED)).await.expect("a frame in time").unwrap();
            if matches!(message.header, Header::Frame { .. }) {
                return (message.header, message.payload);
            }
        }
    }

    async fn subscribe(framed: &mut Framed, max: (u32, u32), fps: u32) {
        let request = Header::Req {
            id: Uuid::new_v4(),
            action: "subscribe".into(),
            args: args(&[
                ("maxWidth", &max.0.to_string()),
                ("maxHeight", &max.1.to_string()),
                ("fps", &fps.to_string()),
            ]),
            deadline_ms: 5000,
        };
        framed.write(&request, &[]).await.unwrap();
    }

    #[tokio::test]
    async fn subscribing_sends_a_scaled_first_frame_immediately_and_then_only_changes() {
        let f = fixture(320, 200, false).await;
        let mut framed = raw_connect(f.address, &SECRET).await;
        subscribe(&mut framed, (160, 160), 30).await;
        let (header, payload) = next_frame(&mut framed).await;
        let Header::Frame { seq, format, width, height, bytes_per_row, screen_width, screen_height, dpi } = header
        else {
            panic!()
        };
        assert_eq!((seq, format.as_str()), (1, "bgra-lz4"));
        assert_eq!((width, height, bytes_per_row), (160, 100, 640));
        assert_eq!((screen_width, screen_height, dpi), (320, 200, 96));
        assert_eq!(decompress(&payload, width, height, bytes_per_row).unwrap().len(), 160 * 100 * 4);
        framed.write(&Header::Ack { seq: 1 }, &[]).await.unwrap();

        // The screen does not change, so nothing more arrives.
        let quiet = tokio::time::timeout(Duration::from_millis(400), next_frame(&mut framed)).await;
        assert!(quiet.is_err(), "no frame for an unchanged screen");

        // A change produces the next frame.
        *f.desktop.changing.lock().unwrap() = true;
        let (header, _) = next_frame(&mut framed).await;
        assert!(matches!(header, Header::Frame { seq: 2, .. }));
    }

    #[tokio::test]
    async fn at_most_two_frames_are_in_flight_without_acknowledgements() {
        let f = fixture(64, 48, false).await;
        *f.desktop.changing.lock().unwrap() = true;
        let mut framed = raw_connect(f.address, &SECRET).await;
        subscribe(&mut framed, (64, 48), 30).await;
        let mut seqs = Vec::new();
        while let Ok(frame) = tokio::time::timeout(Duration::from_millis(600), next_frame(&mut framed)).await {
            let Header::Frame { seq, .. } = frame.0 else { unreachable!() };
            seqs.push(seq);
        }
        assert_eq!(seqs, vec![1, 2], "the third frame waits for an ack");
        framed.write(&Header::Ack { seq: 2 }, &[]).await.unwrap();
        let (header, _) = next_frame(&mut framed).await;
        assert!(matches!(header, Header::Frame { seq: 3, .. }));
    }

    #[tokio::test]
    async fn resubscribing_resizes_and_unsubscribing_stops_frames() {
        let f = fixture(320, 200, false).await;
        let mut framed = raw_connect(f.address, &SECRET).await;
        subscribe(&mut framed, (320, 200), 30).await;
        let (header, _) = next_frame(&mut framed).await;
        assert!(matches!(header, Header::Frame { width: 320, height: 200, .. }));
        framed.write(&Header::Ack { seq: 1 }, &[]).await.unwrap();
        subscribe(&mut framed, (80, 80), 30).await;
        let (header, _) = next_frame(&mut framed).await;
        assert!(matches!(header, Header::Frame { width: 80, height: 50, .. }), "{header:?}");
        let Header::Frame { seq, .. } = header else { unreachable!() };
        framed.write(&Header::Ack { seq }, &[]).await.unwrap();

        let request =
            Header::Req { id: Uuid::new_v4(), action: "unsubscribe".into(), args: BTreeMap::new(), deadline_ms: 5000 };
        framed.write(&request, &[]).await.unwrap();
        *f.desktop.changing.lock().unwrap() = true;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            match tokio::time::timeout_at(deadline, framed.read(Limits::AUTHENTICATED)).await {
                Err(_) => break,
                Ok(message) => {
                    let message = message.unwrap();
                    if let Header::Rep { .. } = message.header {
                        // Frames queued before the unsubscribe was handled may still arrive; none after it.
                        let after = tokio::time::timeout(Duration::from_millis(400), next_frame(&mut framed)).await;
                        assert!(after.is_err(), "no frames after unsubscribe");
                        break;
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn frames_are_not_sent_while_the_session_is_locked() {
        let f = fixture(64, 48, false).await;
        *f.desktop.session.lock().unwrap() = Session::Locked;
        let mut framed = raw_connect(f.address, &SECRET).await;
        subscribe(&mut framed, (64, 48), 30).await;
        let quiet = tokio::time::timeout(Duration::from_millis(400), next_frame(&mut framed)).await;
        assert!(quiet.is_err());
        *f.desktop.session.lock().unwrap() = Session::Ready;
        next_frame(&mut framed).await;
    }

    #[tokio::test]
    async fn the_connection_limit_refuses_extra_peers_without_blocking_the_agent() {
        let f = fixture(64, 48, false).await;
        let mut idle = Vec::new();
        for _ in 0..MAX_CONNECTIONS + 2 {
            idle.push(TcpStream::connect(f.address).await.unwrap());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(f.events.lock().unwrap().iter().any(|e| e.contains("Too many connections")));
        drop(idle);
        tokio::time::sleep(Duration::from_millis(400)).await;
        client(f.address).await.call("ping", BTreeMap::new(), T).await.unwrap();
    }
}
