use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Each variant maps to a process exit code (PRD section 10.4).
#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Prerequisite(String),
    #[error("VM '{0}' does not exist. Run: terminal-windows up {0} --iso PATH")]
    NoSuchVm(String),
    #[error("VM '{0}' is stopped. Run: terminal-windows start {0}")]
    NotRunning(String),
    #[error("{0}")]
    GuestNotReady(String),
    #[error("{0}")]
    AccessDenied(String),
    #[error("{0}")]
    Locked(String),
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            Error::Prerequisite(_) => 3,
            Error::NoSuchVm(_) => 4,
            Error::NotRunning(_) => 5,
            Error::GuestNotReady(_) => 6,
            Error::AccessDenied(_) => 7,
            Error::Locked(_) => 8,
            Error::Other(_) | Error::Io(_) | Error::Json(_) => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_the_prd_table() {
        assert_eq!(Error::Usage(String::new()).exit_code(), 2);
        assert_eq!(Error::Prerequisite(String::new()).exit_code(), 3);
        assert_eq!(Error::NoSuchVm("a".into()).exit_code(), 4);
        assert_eq!(Error::NotRunning("a".into()).exit_code(), 5);
        assert_eq!(Error::GuestNotReady(String::new()).exit_code(), 6);
        assert_eq!(Error::AccessDenied(String::new()).exit_code(), 7);
        assert_eq!(Error::Locked(String::new()).exit_code(), 8);
        assert_eq!(Error::Other(String::new()).exit_code(), 1);
    }
}
