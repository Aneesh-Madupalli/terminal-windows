//! Reads and writes registry values in `HKEY_LOCAL_MACHINE`.

use std::ptr::null_mut;

use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn read_string(subkey: &str, name: &str) -> Option<String> {
    let subkey = wide(subkey);
    let name = wide(name);
    let mut size = 0u32;
    // SAFETY: all pointers are valid for the call; a null data pointer asks for the required size.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        )
    };
    if status != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; size as usize / 2 + 1];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: `buffer` has `size` bytes of room.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_value_that_always_exists() {
        let build = read_string(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuild").unwrap();
        assert!(build.parse::<u32>().is_ok(), "{build}");
    }

    #[test]
    fn missing_values_are_none() {
        assert_eq!(read_string(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "NoSuchValueHere"), None);
        assert_eq!(read_string(r"SOFTWARE\NoSuchKeyHere", "x"), None);
    }
}
