use serde::{Deserialize, Serialize};

/// Guest session state reported by the agent's `status` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestReadiness {
    Unavailable,
    NoSession,
    Locked,
    SecureDesktop,
    CaptureError,
    Ready,
}

impl GuestReadiness {
    /// `None` means the agent did not answer.
    pub fn from_session(session: Option<&str>) -> Self {
        match session {
            None => Self::Unavailable,
            Some("ready") => Self::Ready,
            Some("no-session") => Self::NoSession,
            Some("locked") => Self::Locked,
            Some("secure-desktop") => Self::SecureDesktop,
            Some("capture-error") => Self::CaptureError,
            Some(_) => Self::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_session_strings() {
        assert_eq!(GuestReadiness::from_session(None), GuestReadiness::Unavailable);
        assert_eq!(GuestReadiness::from_session(Some("ready")), GuestReadiness::Ready);
        assert_eq!(GuestReadiness::from_session(Some("locked")), GuestReadiness::Locked);
        assert_eq!(GuestReadiness::from_session(Some("???")), GuestReadiness::Unavailable);
    }

    #[test]
    fn serializes_kebab_case() {
        assert_eq!(serde_json::to_string(&GuestReadiness::SecureDesktop).unwrap(), "\"secure-desktop\"");
    }
}
