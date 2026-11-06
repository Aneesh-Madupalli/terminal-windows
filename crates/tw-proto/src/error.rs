use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("bad magic: not a terminal-windows message")]
    BadMagic,
    #[error("header too large: {0} bytes")]
    HeaderTooLarge(usize),
    #[error("payload too large: {0} bytes")]
    PayloadTooLarge(usize),
    #[error("invalid header: {0}")]
    InvalidHeader(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("authentication failed")]
    AuthFailed,
    #[error("unsupported protocol version")]
    UnsupportedProtocol,
    #[error("unexpected message: expected {0}")]
    Unexpected(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Stable machine codes carried in `values.code` of an error reply (TDD section 8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    BadRequest,
    UnknownAction,
    NoSession,
    Locked,
    SecureDesktop,
    CaptureFailed,
    InputFailed,
    Busy,
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad-request",
            ErrorCode::UnknownAction => "unknown-action",
            ErrorCode::NoSession => "no-session",
            ErrorCode::Locked => "locked",
            ErrorCode::SecureDesktop => "secure-desktop",
            ErrorCode::CaptureFailed => "capture-failed",
            ErrorCode::InputFailed => "input-failed",
            ErrorCode::Busy => "busy",
            ErrorCode::Internal => "internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_kebab_case() {
        assert_eq!(ErrorCode::SecureDesktop.as_str(), "secure-desktop");
        assert_eq!(ErrorCode::BadRequest.as_str(), "bad-request");
    }
}
