//! The guest account's password, kept in Windows Credential Manager so `credential show` can print it later.

use tw_core::Result;

#[cfg(windows)]
mod platform {
    use std::ptr::null_mut;

    use tw_core::{Error, Result};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn write(target: &str, user: &str, password: &str) -> Result<()> {
        let mut target = wide(target);
        let mut user = wide(user);
        let mut blob = password.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            Comment: null_mut(),
            LastWritten: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: null_mut(),
            TargetAlias: null_mut(),
            UserName: user.as_mut_ptr(),
        };
        // SAFETY: every pointer in `credential` refers to a buffer that outlives the call.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(Error::Other(format!("Cannot store the credential: {}", std::io::Error::last_os_error())));
        }
        Ok(())
    }

    pub fn read(target: &str) -> Result<Option<(String, String)>> {
        let target = wide(target);
        let mut found: *mut CREDENTIALW = null_mut();
        // SAFETY: `target` is NUL-terminated and `found` receives a system-allocated credential.
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut found) } == 0 {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(1168) {
                Ok(None) // ERROR_NOT_FOUND
            } else {
                Err(Error::Other(format!("Cannot read the credential: {error}")))
            };
        }
        // SAFETY: on success `found` points at a valid credential until CredFree.
        let result = unsafe {
            let credential = &*found;
            let blob = std::slice::from_raw_parts(credential.CredentialBlob, credential.CredentialBlobSize as usize);
            let mut length = 0;
            while *credential.UserName.add(length) != 0 {
                length += 1;
            }
            let user = String::from_utf16_lossy(std::slice::from_raw_parts(credential.UserName, length));
            (user, String::from_utf8_lossy(blob).into_owned())
        };
        // SAFETY: the buffer came from CredReadW and is not used again.
        unsafe { CredFree(found.cast_const().cast()) };
        Ok(Some(result))
    }

    pub fn delete(target: &str) -> Result<()> {
        let target = wide(target);
        // SAFETY: `target` is NUL-terminated.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(1168) {
                return Err(Error::Other(format!("Cannot delete the credential: {error}")));
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use tw_core::{Error, Result};

    pub fn write(_: &str, _: &str, _: &str) -> Result<()> {
        Err(Error::Prerequisite("Credential Manager needs Windows".into()))
    }

    pub fn read(_: &str) -> Result<Option<(String, String)>> {
        Err(Error::Prerequisite("Credential Manager needs Windows".into()))
    }

    pub fn delete(_: &str) -> Result<()> {
        Err(Error::Prerequisite("Credential Manager needs Windows".into()))
    }
}

pub fn store(target: &str, user: &str, password: &str) -> Result<()> {
    platform::write(target, user, password)
}

/// `(user, password)`, or `None` if nothing is stored.
pub fn fetch(target: &str) -> Result<Option<(String, String)>> {
    platform::read(target)
}

pub fn remove(target: &str) -> Result<()> {
    platform::delete(target)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn store_fetch_and_remove_round_trip() {
        let target = format!("terminal-windows-test/{}/agent", std::process::id());
        assert_eq!(fetch(&target).unwrap(), None);
        store(&target, "agent", "Pa55-w\u{f6}rd_x.").unwrap();
        assert_eq!(fetch(&target).unwrap(), Some(("agent".into(), "Pa55-w\u{f6}rd_x.".into())));
        store(&target, "agent", "second").unwrap();
        assert_eq!(fetch(&target).unwrap().unwrap().1, "second", "storing again replaces");
        remove(&target).unwrap();
        assert_eq!(fetch(&target).unwrap(), None);
        remove(&target).unwrap();
    }
}
