use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;

use crate::error::{Error, Result};
use crate::name::validate_name;

/// Strict option parser: unknown and duplicate options are errors, `--` ends option parsing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Arguments {
    pub positionals: Vec<String>,
    pub options: BTreeMap<String, String>,
    pub flags: BTreeSet<String>,
}

impl Arguments {
    pub fn parse<S: AsRef<str>>(input: &[S], flags: &[&str], options: &[&str]) -> Result<Self> {
        let mut out = Self::default();
        let mut i = 0;
        while i < input.len() {
            let item = input[i].as_ref();
            if item == "--" {
                out.positionals.extend(input[i + 1..].iter().map(|s| s.as_ref().to_string()));
                break;
            }
            if let Some(name) = item.strip_prefix("--") {
                if out.flags.contains(name) || out.options.contains_key(name) {
                    return Err(Error::Usage(format!("Duplicate option: {item}")));
                }
                if flags.contains(&name) {
                    out.flags.insert(name.to_string());
                } else if options.contains(&name) {
                    i += 1;
                    match input.get(i).map(AsRef::as_ref) {
                        Some(value) if !value.starts_with("--") => {
                            out.options.insert(name.to_string(), value.to_string());
                        }
                        _ => return Err(Error::Usage(format!("Missing value for {item}"))),
                    }
                } else {
                    return Err(Error::Usage(format!("Unknown option: {item}")));
                }
            } else {
                out.positionals.push(item.to_string());
            }
            i += 1;
        }
        Ok(out)
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    pub fn option(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    pub fn integer(&self, key: &str, default: i64, range: RangeInclusive<i64>) -> Result<i64> {
        let Some(raw) = self.options.get(key) else { return Ok(default) };
        match raw.parse::<i64>() {
            Ok(value) if range.contains(&value) => Ok(value),
            _ => Err(Error::Usage(format!("--{key} must be in {}..={}", range.start(), range.end()))),
        }
    }

    /// The single optional VM name positional, defaulting to `default`.
    pub fn machine_name(&self) -> Result<String> {
        if self.positionals.len() > 1 {
            return Err(Error::Usage("Expected one VM name".into()));
        }
        let name = self.positionals.first().map_or("default", String::as_str);
        validate_name(name)?;
        Ok(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[&str]) -> Result<Arguments> {
        Arguments::parse(input, &["offline", "force"], &["cpus", "dy", "iso"])
    }

    #[test]
    fn parses_flags_options_and_positionals() {
        let a = parse(&["work", "--offline", "--cpus", "4"]).unwrap();
        assert_eq!(a.positionals, ["work"]);
        assert!(a.flag("offline"));
        assert_eq!(a.option("cpus"), Some("4"));
    }

    #[test]
    fn negative_numbers_are_values() {
        let a = parse(&["--dy", "-300"]).unwrap();
        assert_eq!(a.integer("dy", 0, -1000..=1000).unwrap(), -300);
    }

    #[test]
    fn rejects_unknown_duplicate_and_missing() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--offline", "--offline"]).is_err());
        assert!(parse(&["--cpus", "2", "--cpus", "3"]).is_err());
        assert!(parse(&["--cpus"]).is_err());
        assert!(parse(&["--cpus", "--offline"]).is_err());
    }

    #[test]
    fn double_dash_ends_options() {
        let a = parse(&["--", "--offline", "x"]).unwrap();
        assert_eq!(a.positionals, ["--offline", "x"]);
        assert!(!a.flag("offline"));
    }

    #[test]
    fn integer_enforces_range() {
        let a = parse(&["--cpus", "99"]).unwrap();
        assert!(a.integer("cpus", 4, 2..=8).is_err());
        let b = parse(&["--cpus", "x"]).unwrap();
        assert!(b.integer("cpus", 4, 2..=8).is_err());
        assert_eq!(parse(&[]).unwrap().integer("cpus", 4, 2..=8).unwrap(), 4);
    }

    #[test]
    fn machine_name_defaults_and_validates() {
        assert_eq!(parse(&[]).unwrap().machine_name().unwrap(), "default");
        assert_eq!(parse(&["work"]).unwrap().machine_name().unwrap(), "work");
        assert!(parse(&["a", "b"]).unwrap().machine_name().is_err());
        assert!(parse(&["bad name"]).unwrap().machine_name().is_err());
    }
}
