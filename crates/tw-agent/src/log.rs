//! A small rolling log file. Secrets are never passed to it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const KEEP: usize = 3;

pub struct FileLog {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    lock: Mutex<()>,
}

/// `YYYY-MM-DDTHH:MM:SSZ` from a Unix time, without a date library.
pub fn timestamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

impl FileLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_limits(path, MAX_BYTES, KEEP)
    }

    pub fn with_limits(path: impl Into<PathBuf>, max_bytes: u64, keep: usize) -> Self {
        Self { path: path.into(), max_bytes, keep: keep.max(1), lock: Mutex::new(()) }
    }

    fn rotated(&self, index: usize) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    fn rotate(&self) {
        let _ = fs::remove_file(self.rotated(self.keep - 1));
        for index in (1..self.keep - 1).rev() {
            let _ = fs::rename(self.rotated(index), self.rotated(index + 1));
        }
        let _ = fs::rename(&self.path, self.rotated(1));
    }

    pub fn write(&self, level: &str, message: &str) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if fs::metadata(&self.path).is_ok_and(|m| m.len() >= self.max_bytes) {
            self.rotate();
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let line = format!("{} {level} {}\n", timestamp(now), message.replace(['\r', '\n'], " "));
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = file.write_all(line.as_bytes());
        }
    }

    pub fn info(&self, message: &str) {
        self.write("INFO", message);
    }

    pub fn error(&self, message: &str) {
        self.write("ERROR", message);
    }

    /// The last `lines` lines, oldest first.
    pub fn tail(&self, lines: usize) -> String {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        read_tail(&self.path, lines)
    }
}

fn read_tail(path: &Path, lines: usize) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_correct() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(timestamp(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(timestamp(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn writes_lines_and_returns_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let log = FileLog::new(dir.path().join("logs").join("agent.log"));
        for i in 0..10 {
            log.info(&format!("line {i}"));
        }
        let tail = log.tail(3);
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[2].ends_with("INFO line 9") && lines[0].ends_with("INFO line 7"), "{tail}");
        assert_eq!(log.tail(100).lines().count(), 10);
    }

    #[test]
    fn newlines_in_a_message_cannot_forge_log_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = FileLog::new(dir.path().join("agent.log"));
        log.error("one\nINFO forged\r\nline");
        assert_eq!(log.tail(10).lines().count(), 1);
    }

    #[test]
    fn rotation_keeps_a_bounded_number_of_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.log");
        let log = FileLog::with_limits(&path, 200, 3);
        for i in 0..100 {
            log.info(&format!("message number {i} with some padding to fill the file"));
        }
        assert!(path.exists());
        assert!(log.rotated(1).exists() && log.rotated(2).exists());
        assert!(!log.rotated(3).exists(), "only three files are kept");
        assert!(log.tail(1).contains("number 99"));
    }

    #[test]
    fn a_missing_log_has_an_empty_tail() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(FileLog::new(dir.path().join("none.log")).tail(5), "");
    }
}
