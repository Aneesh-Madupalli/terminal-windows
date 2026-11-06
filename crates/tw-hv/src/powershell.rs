use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tw_core::{Error, Result};

/// Above this many characters of encoded script the command line gets too close to the Windows limit.
const MAX_ENCODED: usize = 24_000;

/// Runs fixed PowerShell scripts. Parameters travel as base64 UTF-8 JSON on stdin, never in script text,
/// so no user-controlled value can change what the script does.
#[derive(Debug, Clone)]
pub struct PowerShell {
    executable: String,
    timeout: Duration,
}

impl Default for PowerShell {
    fn default() -> Self {
        Self { executable: "powershell.exe".into(), timeout: Duration::from_secs(120) }
    }
}

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    result: Option<Value>,
    code: Option<String>,
    message: Option<String>,
}

const PREAMBLE: &str = r#"$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
try {
  $raw = [Console]::In.ReadToEnd().Trim()
  $p = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($raw)) | ConvertFrom-Json
  $result = & {
"#;

const EPILOGUE: &str = r#"
  }
  @{ ok = $true; result = $result } | ConvertTo-Json -Compress -Depth 8
} catch {
  $m = $_.Exception.Message
  $code = 'error'
  if ($m -match '^ELEVATION') { $code = 'access-denied'; $m = $m.Substring(9).Trim() }
  elseif ($m -match 'not recognized as the name of a cmdlet') { $code = 'prerequisite'; $m = 'The Hyper-V PowerShell module is not installed. Run: terminal-windows setup-host --enable-hyper-v' }
  elseif ($m -match 'required permission|Access is denied|authorization policy') { $code = 'access-denied' }
  @{ ok = $false; code = $code; message = $m } | ConvertTo-Json -Compress
}
"#;

pub fn wrap(body: &str) -> String {
    format!("{PREAMBLE}{body}{EPILOGUE}")
}

/// `-EncodedCommand` takes base64 of UTF-16LE text.
pub fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    STANDARD.encode(bytes)
}

fn encode_params(params: &Value) -> String {
    STANDARD.encode(params.to_string())
}

fn parse_output(stdout: &str, stderr: &str) -> Result<Value> {
    let text = stdout.trim_start_matches('\u{feff}');
    let line = text.lines().rev().map(str::trim).find(|line| line.starts_with('{'));
    let Some(line) = line else {
        let detail: String = stderr.trim().chars().take(500).collect();
        return Err(Error::Other(format!("PowerShell returned no result. {detail}").trim().to_string()));
    };
    let envelope: Envelope = serde_json::from_str(line)?;
    if envelope.ok {
        return Ok(envelope.result.unwrap_or(Value::Null));
    }
    let message = envelope.message.unwrap_or_else(|| "PowerShell failed".into());
    Err(match envelope.code.as_deref() {
        Some("access-denied") => Error::AccessDenied(message),
        Some("prerequisite") => Error::Prerequisite(message),
        _ => Error::Other(message),
    })
}

impl PowerShell {
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Runs `body` (which can read `$p` and returns its result) and returns the decoded result.
    pub async fn run(&self, body: &str, params: &Value) -> Result<Value> {
        let script = wrap(body);
        let encoded = encode_command(&script);
        let mut command = Command::new(&self.executable);
        command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"]);

        // A script too long for the command line is run from a temporary file instead.
        let _script_file = if encoded.len() > MAX_ENCODED {
            // The handle must be closed so PowerShell can read the file.
            let path = tempfile::Builder::new().suffix(".ps1").tempfile()?.into_temp_path();
            std::fs::write(&path, format!("\u{feff}{script}"))?;
            command.arg("-File").arg(&path);
            Some(path)
        } else {
            command.arg("-EncodedCommand").arg(&encoded);
            None
        };

        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command.spawn().map_err(|e| Error::Prerequisite(format!("Cannot start PowerShell: {e}")))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(encode_params(params).as_bytes()).await?;
        drop(stdin);

        let output = tokio::time::timeout(self.timeout, child.wait_with_output()).await.map_err(|_| {
            Error::Other(format!("PowerShell did not finish within {} seconds", self.timeout.as_secs()))
        })??;
        parse_output(&String::from_utf8_lossy(&output.stdout), &String::from_utf8_lossy(&output.stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encoded_command_is_utf16_base64() {
        // "ab" in UTF-16LE is 61 00 62 00.
        assert_eq!(encode_command("ab"), STANDARD.encode([0x61, 0, 0x62, 0]));
    }

    #[test]
    fn parses_success_and_failure_envelopes() {
        let ok = parse_output("noise\n{\"ok\":true,\"result\":{\"a\":1}}\n", "").unwrap();
        assert_eq!(ok["a"], 1);
        assert_eq!(parse_output("\u{feff}{\"ok\":true,\"result\":null}", "").unwrap(), Value::Null);

        let denied = parse_output(r#"{"ok":false,"code":"access-denied","message":"nope"}"#, "").unwrap_err();
        assert_eq!(denied.exit_code(), 7);
        let missing = parse_output(r#"{"ok":false,"code":"prerequisite","message":"no module"}"#, "").unwrap_err();
        assert_eq!(missing.exit_code(), 3);
        let other = parse_output(r#"{"ok":false,"code":"error","message":"boom"}"#, "").unwrap_err();
        assert_eq!(other.to_string(), "boom");
    }

    #[test]
    fn missing_output_reports_stderr() {
        let error = parse_output("", "Something broke").unwrap_err().to_string();
        assert!(error.contains("Something broke"), "{error}");
    }

    #[cfg(windows)]
    mod live {
        use super::*;

        #[tokio::test]
        async fn parameters_round_trip_including_unicode_and_quotes() {
            let value = "caf\u{e9} \"quoted\" $(whoami) `tick` \u{65e5}\u{672c}\u{8a9e}";
            let result = PowerShell::default().run("$p.value", &json!({ "value": value })).await.unwrap();
            assert_eq!(result, json!(value));
        }

        #[tokio::test]
        async fn script_errors_become_errors() {
            let error = PowerShell::default().run("throw 'kaboom'", &json!({})).await.unwrap_err();
            assert_eq!(error.to_string(), "kaboom");
        }

        #[tokio::test]
        async fn elevation_errors_map_to_access_denied() {
            let error =
                PowerShell::default().run("throw 'ELEVATION Run as administrator'", &json!({})).await.unwrap_err();
            assert_eq!(error.exit_code(), 7);
            assert_eq!(error.to_string(), "Run as administrator");
        }

        #[tokio::test]
        async fn a_hung_script_is_killed_at_the_timeout() {
            let started = std::time::Instant::now();
            let ps = PowerShell::default().with_timeout(Duration::from_millis(1500));
            let error = ps.run("Start-Sleep -Seconds 60", &json!({})).await.unwrap_err();
            assert!(error.to_string().contains("did not finish"), "{error}");
            assert!(started.elapsed() < Duration::from_secs(20));
        }

        #[tokio::test]
        async fn long_scripts_run_from_a_file() {
            let padding = "# padding padding padding padding padding\n".repeat(900);
            let result = PowerShell::default().run(&format!("{padding}$p.n + 1"), &json!({ "n": 41 })).await.unwrap();
            assert_eq!(result, json!(42));
        }
    }
}
