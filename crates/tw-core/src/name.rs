use crate::error::{Error, Result};

const MAX_NAME: usize = 48;

/// `^[a-zA-Z0-9][a-zA-Z0-9_-]{0,47}$`
pub fn validate_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_NAME
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-');
    if valid {
        Ok(())
    } else {
        Err(Error::Usage(
            "VM names must be 1-48 letters, numbers, underscores or hyphens, starting with a letter or number".into(),
        ))
    }
}

/// A per-user tag keeps Hyper-V names unique when several Windows users share one host.
pub fn validate_user_tag(tag: &str) -> Result<()> {
    if tag.len() == 8 && tag.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(Error::Other("user tag must be 8 lowercase hex digits".into()))
    }
}

/// The Hyper-V VM name for `name`: `tw-<name>-<usertag>`.
pub fn hv_name(name: &str, user_tag: &str) -> Result<String> {
    validate_name(name)?;
    validate_user_tag(user_tag)?;
    Ok(format!("tw-{name}-{user_tag}"))
}

/// A NetBIOS-safe guest computer name: no underscores, not all digits, at most 15 characters.
pub fn computer_name(name: &str) -> String {
    let mut out: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '-' }).take(15).collect();
    out = out.trim_matches('-').to_string();
    if out.is_empty() || out.bytes().all(|b| b.is_ascii_digit()) {
        out = format!("TW{out}");
        out.truncate(15);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_names() {
        for name in ["default", "a", "work_1", "a-b", "9lives", &"x".repeat(48)] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_invalid_names() {
        for name in ["", "-a", "_a", "a b", "a\"b", "a`b", "a$(x)", "a/b", "a.b", &"x".repeat(49), "caf\u{e9}"] {
            assert!(validate_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn hv_name_is_namespaced_per_user() {
        assert_eq!(hv_name("default", "3fa9c1d2").unwrap(), "tw-default-3fa9c1d2");
        assert!(hv_name("default", "XYZ").is_err());
        assert!(hv_name("bad name", "3fa9c1d2").is_err());
    }

    #[test]
    fn computer_names_are_netbios_safe() {
        assert_eq!(computer_name("default"), "DEFAULT");
        assert_eq!(computer_name("work_station_long_name"), "WORK-STATION-LO");
        assert_eq!(computer_name("12345"), "TW12345");
        assert_eq!(computer_name("_-"), "TW");
        assert!(computer_name("a_very_long_virtual_machine_name").len() <= 15);
    }
}
