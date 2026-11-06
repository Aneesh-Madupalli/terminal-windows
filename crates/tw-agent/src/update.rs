//! Self-update: stage the new binary, verify it, and swap it in. A running executable can be renamed on
//! Windows, so the old file becomes `twagent.old` and the supervisor can roll back to it.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::server::Updater;

/// Exit code that asks the supervisor to start the (new) agent immediately.
pub const RESTART_EXIT: i32 = 75;

pub struct FileUpdater {
    exe: PathBuf,
    current_build: u32,
}

fn sibling(exe: &Path, extension: &str) -> PathBuf {
    exe.with_extension(extension)
}

pub fn old_path(exe: &Path) -> PathBuf {
    sibling(exe, "old")
}

/// Constant-time comparison of two digests.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

impl FileUpdater {
    pub fn new(exe: impl Into<PathBuf>, current_build: u32) -> Self {
        Self { exe: exe.into(), current_build }
    }
}

impl Updater for FileUpdater {
    fn apply(&self, sha256: &str, build: u32, payload: &[u8]) -> Result<(), String> {
        if build <= self.current_build {
            return Err(format!("Build {build} is not newer than the running build {}", self.current_build));
        }
        if payload.len() < 1024 || &payload[..2] != b"MZ" {
            return Err("The update is not a Windows executable".into());
        }
        if !same(sha256_hex(payload).as_bytes(), sha256.to_ascii_lowercase().as_bytes()) {
            return Err("The update does not match its SHA-256".into());
        }
        let staged = sibling(&self.exe, "new");
        let old = old_path(&self.exe);
        std::fs::write(&staged, payload).map_err(|e| format!("Cannot stage the update: {e}"))?;
        let _ = std::fs::remove_file(&old);
        if let Err(error) = std::fs::rename(&self.exe, &old) {
            let _ = std::fs::remove_file(&staged);
            return Err(format!("Cannot move the current agent aside: {error}"));
        }
        if let Err(error) = std::fs::rename(&staged, &self.exe) {
            let _ = std::fs::rename(&old, &self.exe);
            return Err(format!("Cannot install the update: {error}"));
        }
        Ok(())
    }

    fn restart(&self) {
        std::process::exit(RESTART_EXIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(tag: u8) -> Vec<u8> {
        let mut bytes = vec![tag; 4096];
        bytes[..2].copy_from_slice(b"MZ");
        bytes
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, FileUpdater) {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("twagent.exe");
        std::fs::write(&exe, executable(1)).unwrap();
        let updater = FileUpdater::new(&exe, 1);
        (dir, exe, updater)
    }

    #[test]
    fn a_valid_update_swaps_the_binary_and_keeps_the_old_one() {
        let (_dir, exe, updater) = fixture();
        let new = executable(2);
        updater.apply(&sha256_hex(&new), 2, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), new);
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), executable(1));
        assert!(!sibling(&exe, "new").exists());
    }

    #[test]
    fn a_wrong_hash_changes_nothing() {
        let (_dir, exe, updater) = fixture();
        let new = executable(2);
        let error = updater.apply(&sha256_hex(b"other"), 2, &new).unwrap_err();
        assert!(error.contains("SHA-256"), "{error}");
        assert_eq!(std::fs::read(&exe).unwrap(), executable(1));
        assert!(!old_path(&exe).exists() && !sibling(&exe, "new").exists());
    }

    #[test]
    fn hashes_are_compared_without_case_sensitivity() {
        let (_dir, _exe, updater) = fixture();
        let new = executable(3);
        updater.apply(&sha256_hex(&new).to_uppercase(), 2, &new).unwrap();
    }

    #[test]
    fn old_builds_and_non_executables_are_refused() {
        let (_dir, exe, updater) = fixture();
        let new = executable(2);
        assert!(updater.apply(&sha256_hex(&new), 1, &new).is_err(), "same build");
        assert!(updater.apply(&sha256_hex(&new), 0, &new).is_err(), "older build");
        let text = vec![b'a'; 4096];
        assert!(updater.apply(&sha256_hex(&text), 2, &text).is_err());
        let tiny = b"MZ".to_vec();
        assert!(updater.apply(&sha256_hex(&tiny), 2, &tiny).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), executable(1));
    }

    #[test]
    fn sha256_matches_the_known_digest() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn digest_comparison_rejects_length_and_content_differences() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
    }
}
