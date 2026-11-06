//! The per-VM channel secret. On Windows it is stored encrypted with DPAPI, so only the same user on the same
//! machine can read it back.

use std::path::Path;

use crate::error::{Error, Result};

pub const SECRET_LEN: usize = 32;

/// A fresh random secret.
pub fn generate() -> [u8; SECRET_LEN] {
    let mut secret = [0u8; SECRET_LEN];
    getrandom::fill(&mut secret).expect("the operating system random source is available");
    secret
}

#[cfg(windows)]
mod platform {
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData};

    use crate::error::{Error, Result};

    /// Ties the blob to this application as well as to the user.
    const ENTROPY: &[u8] = b"terminal-windows channel secret v1";

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr().cast_mut() }
    }

    fn transform(data: &[u8], protect: bool) -> Result<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
        // SAFETY: the blobs point at live slices for the duration of the call, and `output` receives a buffer
        // that is copied and then released with LocalFree.
        let ok = unsafe {
            if protect {
                CryptProtectData(&input, null(), &entropy, null(), null(), 0, &mut output)
            } else {
                CryptUnprotectData(&input, null_mut(), &entropy, null(), null(), 0, &mut output)
            }
        };
        if ok == 0 {
            return Err(Error::Other(format!("Windows data protection failed: {}", std::io::Error::last_os_error())));
        }
        // SAFETY: on success `pbData` points at `cbData` bytes allocated by the system.
        let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        // SAFETY: the buffer came from the system allocator and is not used again.
        unsafe { LocalFree(output.pbData.cast()) };
        Ok(bytes)
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        transform(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        transform(data, false)
    }
}

/// Other platforms have no DPAPI. This keeps the workspace testable there; the product runs on Windows.
#[cfg(not(windows))]
mod platform {
    use crate::error::Result;

    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }
}

pub use platform::{protect, unprotect};

/// Encrypts and writes atomically.
pub fn save(path: &Path, secret: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, protect(secret)?)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Vec<u8>> {
    let blob = std::fs::read(path)
        .map_err(|e| Error::Other(format!("Cannot read the channel secret {}: {e}", path.display())))?;
    let secret = unprotect(&blob)?;
    if secret.len() != SECRET_LEN {
        return Err(Error::Other("The channel secret file is damaged".into()));
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_random_and_the_right_size() {
        assert_ne!(generate(), generate());
        assert_eq!(generate().len(), SECRET_LEN);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.secret");
        let secret = generate();
        save(&path, &secret).unwrap();
        assert_eq!(load(&path).unwrap(), secret);
    }

    #[test]
    fn a_missing_or_damaged_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("none")).is_err());
        let path = dir.path().join("bad");
        save(&path, &[1, 2, 3]).unwrap();
        assert!(load(&path).is_err(), "wrong length");
    }

    #[cfg(windows)]
    #[test]
    fn the_stored_bytes_are_encrypted_and_tamper_evident() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.secret");
        let secret = [0x5Au8; SECRET_LEN];
        save(&path, &secret).unwrap();
        let mut stored = std::fs::read(&path).unwrap();
        assert!(!stored.windows(SECRET_LEN).any(|w| w == secret), "the secret is not stored in clear");
        let middle = stored.len() / 2;
        stored[middle] ^= 0xFF;
        std::fs::write(&path, stored).unwrap();
        assert!(load(&path).is_err());
    }
}
