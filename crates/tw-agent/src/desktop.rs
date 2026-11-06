//! What the agent needs from the machine it runs on. The Windows implementation is in `win`; tests use a fake.

use tw_proto::ErrorCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Ready,
    NoSession,
    Locked,
    SecureDesktop,
    CaptureError,
}

impl Session {
    pub const fn as_str(self) -> &'static str {
        match self {
            Session::Ready => "ready",
            Session::NoSession => "no-session",
            Session::Locked => "locked",
            Session::SecureDesktop => "secure-desktop",
            Session::CaptureError => "capture-error",
        }
    }

    /// The error an action gets when the session is not usable.
    pub fn refusal(self) -> Option<DesktopError> {
        let (code, message) = match self {
            Session::Ready => return None,
            Session::NoSession => (ErrorCode::NoSession, "No user is signed in on the guest desktop"),
            Session::Locked => {
                (ErrorCode::Locked, "The guest is locked. Unlock it from the viewer or the guest sign-in screen")
            }
            Session::SecureDesktop => (
                ErrorCode::SecureDesktop,
                "The secure desktop (a UAC prompt or sign-in screen) is active and cannot be controlled from inside the guest",
            ),
            Session::CaptureError => (ErrorCode::CaptureFailed, "The guest screen cannot be captured right now"),
        };
        Some(DesktopError::new(code, message))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
}

/// Top-down 32-bit BGRA pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: Vec<u8>,
}

impl Bitmap {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self { width, height, stride: width as usize * 4, data }
    }

    pub fn row(&self, y: u32) -> &[u8] {
        let start = y as usize * self.stride;
        &self.data[start..start + self.width as usize * 4]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopError {
    pub code: ErrorCode,
    pub message: String,
}

impl DesktopError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for DesktopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DesktopError {}

/// One low-level input event. Positions are screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    MoveTo { x: u32, y: u32 },
    Button { right: bool, down: bool },
    Wheel { horizontal: bool, delta: i32 },
    Key { vk: u16, extended: bool, down: bool },
    Unicode { unit: u16, down: bool },
}

pub trait Desktop: Send + Sync {
    fn session(&self) -> Session;
    fn screen(&self) -> Result<Screen, DesktopError>;
    /// The primary display at pixel size, with the pointer drawn in.
    fn capture(&self) -> Result<Bitmap, DesktopError>;
    /// Injects the events in order, as one batch.
    fn send(&self, ops: &[Op]) -> Result<(), DesktopError>;
    fn os(&self) -> String;
}

#[cfg(test)]
pub(crate) mod fake {
    use std::sync::Mutex;

    use super::*;

    /// A desktop that records input and serves a changing test pattern.
    pub struct FakeDesktop {
        pub session: Mutex<Session>,
        pub screen: Screen,
        pub sent: Mutex<Vec<Op>>,
        pub captures: Mutex<u32>,
        /// When false, every capture returns the same pixels.
        pub changing: Mutex<bool>,
        pub fail_send: Mutex<bool>,
    }

    impl FakeDesktop {
        pub fn new(width: u32, height: u32) -> Self {
            Self {
                session: Mutex::new(Session::Ready),
                screen: Screen { width, height, dpi: 96 },
                sent: Mutex::new(Vec::new()),
                captures: Mutex::new(0),
                changing: Mutex::new(false),
                fail_send: Mutex::new(false),
            }
        }
    }

    impl Desktop for FakeDesktop {
        fn session(&self) -> Session {
            *self.session.lock().unwrap()
        }

        fn screen(&self) -> Result<Screen, DesktopError> {
            Ok(self.screen)
        }

        fn capture(&self) -> Result<Bitmap, DesktopError> {
            let mut captures = self.captures.lock().unwrap();
            *captures += 1;
            let shade = if *self.changing.lock().unwrap() { *captures as u8 } else { 7 };
            let (w, h) = (self.screen.width, self.screen.height);
            let mut data = vec![0u8; (w * h * 4) as usize];
            for (i, pixel) in data.chunks_exact_mut(4).enumerate() {
                pixel.copy_from_slice(&[(i % w as usize) as u8, shade, (i / w as usize) as u8, 255]);
            }
            Ok(Bitmap::new(w, h, data))
        }

        fn send(&self, ops: &[Op]) -> Result<(), DesktopError> {
            if *self.fail_send.lock().unwrap() {
                return Err(DesktopError::new(ErrorCode::InputFailed, "injected fewer events than requested"));
            }
            self.sent.lock().unwrap().extend_from_slice(ops);
            Ok(())
        }

        fn os(&self) -> String {
            "Fake OS".into()
        }
    }
}
