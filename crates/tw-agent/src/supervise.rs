//! Keeps the agent running: restarts it when it exits, and rolls back to `twagent.old` when a freshly
//! installed build keeps crashing right after it starts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

use crate::update::{RESTART_EXIT, old_path};

/// A run shorter than this counts as a failed start.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(20);
const MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Restart(Duration),
    Rollback,
}

#[derive(Debug, Default)]
pub struct Policy {
    quick_failures: u32,
}

impl Policy {
    /// What to do after the agent exited with `code` following `uptime`.
    pub fn on_exit(&mut self, code: Option<i32>, uptime: Duration, rollback_possible: bool) -> Next {
        if code == Some(RESTART_EXIT) {
            self.quick_failures = 0;
            return Next::Restart(Duration::ZERO);
        }
        if uptime >= HEALTHY_AFTER {
            self.quick_failures = 0;
            return Next::Restart(Duration::from_secs(1));
        }
        self.quick_failures += 1;
        if self.quick_failures >= 2 && rollback_possible {
            self.quick_failures = 0;
            return Next::Rollback;
        }
        Next::Restart(Duration::from_secs(1u64 << self.quick_failures.min(5)).min(MAX_DELAY))
    }
}

/// Puts the previous build back. The failing build is kept as `twagent.bad`.
pub fn rollback(exe: &Path) -> std::io::Result<()> {
    let old = old_path(exe);
    let bad = exe.with_extension("bad");
    let _ = std::fs::remove_file(&bad);
    std::fs::rename(exe, &bad)?;
    if let Err(error) = std::fs::rename(&old, exe) {
        let _ = std::fs::rename(&bad, exe);
        return Err(error);
    }
    Ok(())
}

pub struct Supervisor {
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// Stop after this many runs; `None` runs forever. Used by tests.
    pub max_runs: Option<u32>,
    pub on_event: Box<dyn Fn(&str) + Send + Sync>,
}

impl Supervisor {
    pub async fn run(&self) {
        let mut policy = Policy::default();
        let mut runs = 0u32;
        loop {
            if self.max_runs.is_some_and(|max| runs >= max) {
                return;
            }
            runs += 1;
            let started = std::time::Instant::now();
            let status = match Command::new(&self.exe).args(&self.args).kill_on_drop(true).status().await {
                Ok(status) => status.code(),
                Err(error) => {
                    (self.on_event)(&format!("Cannot start the agent: {error}"));
                    None
                }
            };
            let uptime = started.elapsed();
            (self.on_event)(&format!("The agent exited with {status:?} after {uptime:?}"));
            match policy.on_exit(status, uptime, old_path(&self.exe).exists()) {
                Next::Restart(delay) => tokio::time::sleep(delay).await,
                Next::Rollback => match rollback(&self.exe) {
                    Ok(()) => (self.on_event)("The new agent keeps failing; restored the previous build"),
                    Err(error) => (self.on_event)(&format!("Rollback failed: {error}")),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    const QUICK: Duration = Duration::from_secs(1);
    const LONG: Duration = Duration::from_secs(300);

    #[test]
    fn a_requested_restart_is_immediate_and_not_a_failure() {
        let mut policy = Policy::default();
        for _ in 0..5 {
            assert_eq!(policy.on_exit(Some(RESTART_EXIT), QUICK, true), Next::Restart(Duration::ZERO));
        }
    }

    #[test]
    fn quick_failures_back_off_and_then_roll_back() {
        let mut policy = Policy::default();
        assert_eq!(policy.on_exit(Some(1), QUICK, true), Next::Restart(Duration::from_secs(2)));
        assert_eq!(policy.on_exit(Some(1), QUICK, true), Next::Rollback);
        // The counter starts again after a rollback.
        assert_eq!(policy.on_exit(Some(1), QUICK, true), Next::Restart(Duration::from_secs(2)));
    }

    #[test]
    fn without_a_previous_build_it_only_backs_off() {
        let mut policy = Policy::default();
        let mut last = Duration::ZERO;
        for _ in 0..12 {
            let Next::Restart(delay) = policy.on_exit(Some(1), QUICK, false) else { panic!("no rollback possible") };
            assert!(delay >= last && delay <= MAX_DELAY);
            last = delay;
        }
        assert_eq!(last, Duration::from_secs(30));
    }

    #[test]
    fn a_healthy_run_resets_the_failure_count() {
        let mut policy = Policy::default();
        assert!(matches!(policy.on_exit(Some(1), QUICK, true), Next::Restart(_)));
        assert_eq!(policy.on_exit(Some(0), LONG, true), Next::Restart(Duration::from_secs(1)));
        assert!(matches!(policy.on_exit(Some(1), QUICK, true), Next::Restart(_)), "counting restarted");
    }

    #[test]
    fn rollback_restores_the_previous_build_and_keeps_the_bad_one() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("twagent.exe");
        std::fs::write(&exe, b"new").unwrap();
        std::fs::write(old_path(&exe), b"old").unwrap();
        rollback(&exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert_eq!(std::fs::read(exe.with_extension("bad")).unwrap(), b"new");
        assert!(!old_path(&exe).exists());
    }

    #[test]
    fn a_failed_rollback_leaves_the_current_build_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("twagent.exe");
        std::fs::write(&exe, b"new").unwrap();
        assert!(rollback(&exe).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_supervisor_restarts_a_child_that_exits() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let supervisor = Supervisor {
            exe: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), format!("exit {RESTART_EXIT}")],
            max_runs: Some(3),
            on_event: Box::new(move |message| sink.lock().unwrap().push(message.to_string())),
        };
        tokio::time::timeout(Duration::from_secs(30), supervisor.run()).await.expect("finishes after three runs");
        let events = events.lock().unwrap();
        assert_eq!(events.iter().filter(|e| e.contains("exited with Some(75)")).count(), 3, "{events:?}");
    }
}
