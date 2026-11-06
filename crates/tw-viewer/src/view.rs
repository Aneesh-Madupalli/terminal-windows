//! The interactive view: draws the guest desktop, forwards input, and keeps the link alive.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, interval, sleep_until};
use tw_client::{Frame, FrameStream, GuestError};
use tw_term::capability::{self, Probe, QUERIES};
use tw_term::chrome::Theme;
use tw_term::{Bitmap, ImageLayout, Protocol, TerminalEvent, TerminalInput, TerminalMetricFilter};

use crate::draw::{self, KittyIds, Link};
use crate::mapping::{Geometry, MouseMapper, Request, key_request, text_request};
use crate::session::Session;
use crate::terminal::Terminal;

const SETUP: &str = "\x1b[?1049h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[2J";
const RESTORE: &str = "\x1b[?1002l\x1b[?1006l\x1b[0m\x1b[?25h\x1b[?1049l";
const DEFAULT_CELL: (f64, f64) = (10.0, 20.0);
const FIRST_RETRY: Duration = Duration::from_millis(500);
const LAST_RETRY: Duration = Duration::from_secs(5);
const ESCAPE_WAIT: Duration = Duration::from_millis(40);

pub struct Options {
    pub name: String,
    /// Forces a graphics protocol instead of detecting one.
    pub protocol: Option<Protocol>,
    /// Forces the cell size in pixels, for terminals that do not report it.
    pub cell: Option<(f64, f64)>,
    pub theme: Theme,
    /// How long to wait for the terminal's answers to the start-up queries.
    pub probe_wait: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    Quit,
    InputClosed,
    /// The machine no longer exists.
    Gone(String),
}

struct Viewer {
    session: Arc<dyn Session>,
    term: Arc<dyn Terminal>,
    options: Options,
    protocol: Protocol,
    cell: (f64, f64),
    actions: UnboundedSender<Request>,
    notices: UnboundedSender<String>,
    stream: Option<FrameStream>,
    link: Link,
    geometry: Option<Geometry>,
    mapper: MouseMapper,
    ids: KittyIds,
    menu: bool,
    notice: String,
    size: (usize, usize),
    retry: Duration,
    next_connect: Instant,
    gone: Option<String>,
}

fn pending_forever<T>() -> impl std::future::Future<Output = T> {
    std::future::pending()
}

impl Viewer {
    fn max_pixels(&self) -> (u32, u32) {
        let (columns, rows) = self.size;
        let width = (columns as f64 * self.cell.0) as u32;
        let height = (rows.saturating_sub(1).max(1) as f64 * self.cell.1) as u32;
        (width.clamp(16, 7680), height.clamp(16, 4320))
    }

    fn draw_status(&self) {
        let (columns, rows) = self.size;
        let bar = draw::status_bar(&draw::Bar {
            theme: &self.options.theme,
            columns,
            row: rows,
            link: self.link,
            name: &self.options.name,
            protocol: self.protocol,
            notice: &self.notice,
            menu: self.menu,
        });
        self.term.write(&bar);
    }

    fn send(&self, request: Request) {
        let _ = self.actions.send(request);
    }

    fn explain_notice(&mut self, message: String) {
        self.notice = message;
        self.draw_status();
    }

    async fn connect(&mut self) {
        let (width, height) = self.max_pixels();
        let attempt = async {
            let mut stream = self.session.open_stream().await?;
            stream.subscribe(width, height, self.protocol.target_fps(), Duration::from_secs(5)).await?;
            Ok::<_, GuestError>(stream)
        };
        match attempt.await {
            Ok(stream) => {
                self.stream = Some(stream);
                self.retry = FIRST_RETRY;
                self.notice.clear();
            }
            Err(GuestError::NotCreated(name)) => self.gone = Some(name),
            Err(error) => {
                self.notice = self.session.explain(&error);
                self.link = Link::Lost;
                self.next_connect = Instant::now() + self.retry;
                self.retry = (self.retry * 2).min(LAST_RETRY);
            }
        }
        self.draw_status();
    }

    async fn on_frame(&mut self, frame: Frame) {
        let (columns, rows) = self.size;
        let Some(bitmap) =
            Bitmap::from_bgra(frame.width as usize, frame.height as usize, frame.width as usize * 4, &frame.bgra)
        else {
            return;
        };
        let layout = ImageLayout::new(
            frame.width as usize,
            frame.height as usize,
            1,
            1,
            columns,
            rows.saturating_sub(1).max(1),
            self.cell.0,
            self.cell.1,
        );
        let drawing = draw::frame(self.protocol, &bitmap, &layout, &mut self.ids);
        let term = self.term.clone();
        let _ = tokio::task::spawn_blocking(move || term.write_and_wait(&drawing)).await;
        self.geometry = Some(Geometry {
            layout,
            frame: (frame.width, frame.height),
            screen: (frame.screen_width, frame.screen_height),
        });
        if self.link != Link::Live {
            self.link = Link::Live;
            if self.notice.starts_with("Connection lost") {
                self.notice.clear();
            }
            self.draw_status();
        }
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.ack(frame.seq).await;
        }
    }

    fn lost(&mut self, reason: String) {
        self.stream = None;
        self.link = Link::Lost;
        self.notice = format!("Connection lost: {reason}");
        self.next_connect = Instant::now() + self.retry;
        self.retry = (self.retry * 2).min(LAST_RETRY);
        self.draw_status();
    }

    async fn resubscribe(&mut self) {
        let (width, height) = self.max_pixels();
        let fps = self.protocol.target_fps();
        let Some(stream) = self.stream.as_mut() else { return };
        if let Err(error) = stream.subscribe(width, height, fps, Duration::from_secs(5)).await {
            self.lost(error.to_string());
        }
    }

    async fn check_resize(&mut self) {
        let size = self.term.size();
        if size == self.size {
            return;
        }
        self.size = size;
        self.geometry = None;
        let cleared = self.ids.clear();
        self.term.write(&format!("{cleared}\x1b[2J"));
        self.resubscribe().await;
        self.draw_status();
    }

    fn menu_key(&mut self, key: &str) -> Option<Exit> {
        self.menu = false;
        let session = self.session.clone();
        let notices = self.notices.clone();
        match key {
            "d" => {
                tokio::spawn(async move {
                    let result = session.ctrl_alt_del().await.map(|()| String::new());
                    let _ = notices.send(result.unwrap_or_else(|e| session.explain(&e)));
                });
            }
            "o" => {
                tokio::spawn(async move {
                    let result = session.open_console().await.map(|()| String::new());
                    let _ = notices.send(result.unwrap_or_else(|e| session.explain(&e)));
                });
            }
            "r" => {
                self.geometry = None;
                let cleared = self.ids.clear();
                self.term.write(&format!("{cleared}\x1b[2J"));
                // A fresh subscription makes the agent send the whole screen again.
                self.retry = FIRST_RETRY;
                self.stream = None;
                self.next_connect = Instant::now();
            }
            "q" => return Some(Exit::Quit),
            _ => {}
        }
        self.draw_status();
        None
    }

    fn handle_events(&mut self, events: Vec<TerminalEvent>) -> Option<Exit> {
        let mut text = String::new();
        let flush = |viewer: &Viewer, text: &mut String| {
            if !text.is_empty() {
                viewer.send(text_request(text));
                text.clear();
            }
        };
        for event in events {
            match event {
                TerminalEvent::Quit => return Some(Exit::Quit),
                TerminalEvent::Text(typed) if self.menu => {
                    if let Some(exit) = self.menu_key(&typed) {
                        return Some(exit);
                    }
                }
                TerminalEvent::Text(typed) => text.push_str(&typed),
                TerminalEvent::Key(key) => {
                    flush(self, &mut text);
                    if key == "ctrl+g" {
                        self.menu = !self.menu;
                        self.draw_status();
                    } else if self.menu {
                        if let Some(exit) = self.menu_key(&key) {
                            return Some(exit);
                        }
                    } else if let Some(request) = key_request(&key) {
                        self.send(request);
                    }
                }
                TerminalEvent::Mouse { button, column, row, released } => {
                    flush(self, &mut text);
                    if !self.menu {
                        if let Some(geometry) = self.geometry {
                            if let Some(request) = self.mapper.event(button, column, row, released, &geometry) {
                                self.send(request);
                            }
                        }
                    }
                }
            }
        }
        flush(self, &mut text);
        None
    }
}

async fn probe(
    term: &dyn Terminal,
    input: &mut UnboundedReceiver<Vec<u8>>,
    wait: Duration,
) -> Option<(TerminalMetricFilter, TerminalInput, Vec<TerminalEvent>)> {
    let mut filter = TerminalMetricFilter::new();
    let mut parser = TerminalInput::new();
    let mut early = Vec::new();
    term.write(QUERIES);
    let deadline = Instant::now() + wait;
    loop {
        tokio::select! {
            bytes = input.recv() => {
                let bytes = bytes?;
                let rest = filter.feed(&bytes, false);
                early.extend(parser.feed(&rest, false));
            }
            () = sleep_until(deadline) => break,
        }
    }
    Some((filter, parser, early))
}

/// Runs the view until the user quits, the input closes, or the machine is gone.
pub async fn run(
    session: Arc<dyn Session>,
    term: Arc<dyn Terminal>,
    mut input: UnboundedReceiver<Vec<u8>>,
    options: Options,
) -> Exit {
    term.write(SETUP);
    let Some((mut filter, mut parser, early)) = probe(&*term, &mut input, options.probe_wait).await else {
        term.write(RESTORE);
        return Exit::InputClosed;
    };
    let kitty = filter.graphics_replies.get(&31).is_some_and(|reply| reply == "OK");
    let protocol = options.protocol.unwrap_or_else(|| {
        capability::detect(&|key| std::env::var(key).ok(), Probe { kitty, sixel: filter.supports_sixel() })
    });
    let cell = options.cell.or(filter.cell_width.zip(filter.cell_height)).unwrap_or(DEFAULT_CELL);

    let (actions, mut action_queue) = unbounded_channel::<Request>();
    let (notices, mut notice_queue) = unbounded_channel::<String>();
    let worker = {
        let (session, notices) = (session.clone(), notices.clone());
        tokio::spawn(async move {
            while let Some(request) = action_queue.recv().await {
                let outcome = match session.call(request.action, request.args).await {
                    Ok(()) => String::new(),
                    Err(error) => session.explain(&error),
                };
                let _ = notices.send(outcome);
            }
        })
    };

    let size = term.size();
    let mut viewer = Viewer {
        session,
        term: term.clone(),
        options,
        protocol,
        cell,
        actions,
        notices,
        stream: None,
        link: Link::Connecting,
        geometry: None,
        mapper: MouseMapper::default(),
        ids: KittyIds::default(),
        menu: false,
        notice: String::new(),
        size,
        retry: FIRST_RETRY,
        next_connect: Instant::now(),
        gone: None,
    };
    viewer.draw_status();

    let mut exit = viewer.handle_events(early);
    let mut escape_deadline: Option<Instant> = None;
    let mut ticker = interval(Duration::from_millis(250));
    while exit.is_none() {
        let connect_at = viewer.stream.is_none().then_some(viewer.next_connect);
        tokio::select! {
            bytes = input.recv() => {
                let Some(bytes) = bytes else { exit = Some(Exit::InputClosed); break };
                let rest = filter.feed(&bytes, false);
                let events = parser.feed(&rest, false);
                escape_deadline = (parser.pending() > 0).then(|| Instant::now() + ESCAPE_WAIT);
                exit = viewer.handle_events(events);
            }
            frame = async {
                match viewer.stream.as_mut() {
                    Some(stream) => stream.next_frame().await,
                    None => pending_forever().await,
                }
            } => match frame {
                Ok(frame) => viewer.on_frame(frame).await,
                Err(error) => viewer.lost(error.to_string()),
            },
            () = async {
                match connect_at {
                    Some(at) => sleep_until(at).await,
                    None => pending_forever().await,
                }
            } => {
                viewer.connect().await;
                if let Some(name) = viewer.gone.take() {
                    exit = Some(Exit::Gone(name));
                }
            }
            () = async {
                match escape_deadline {
                    Some(at) => sleep_until(at).await,
                    None => pending_forever().await,
                }
            } => {
                escape_deadline = None;
                let events = parser.feed(&[], true);
                exit = viewer.handle_events(events);
            }
            _ = ticker.tick() => viewer.check_resize().await,
            Some(notice) = notice_queue.recv() => viewer.explain_notice(notice),
        }
    }
    worker.abort();
    let cleared = viewer.ids.clear();
    term.write(&format!("{cleared}{RESTORE}"));
    exit.unwrap_or(Exit::Quit)
}
