# Releasing

## Before tagging

1. Run the checks that need a real hypervisor, as listed in `docs/VALIDATION.md`: `doctor`, `up --iso ...`, `view`, `mcp`, and the VirtualBox and Hyper-V spikes. A tag publishes a build; do not tag a version nobody has run end to end.
2. Raise the version in `Cargo.toml` (`[workspace.package] version`) and `.claude-plugin/plugin.json`.
3. Raise `BUILD` in `crates/tw-agent/src/lib.rs` if the guest agent changed. The host updates an older agent when `agent update` runs, comparing this number.
4. Make sure everything passes locally:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   ```

5. Refresh `THIRD-PARTY-NOTICES.md` if dependencies changed.

## Publishing

```bash
git tag v0.1.0
git push origin v0.1.0
```

The `release` workflow builds on a Windows runner, runs the tests, runs `scripts/build.ps1`, and creates the GitHub release with `terminal-windows-<version>-x64.zip` and its `.sha256` file attached.

To build the same zip locally: `.\scripts\build.ps1` (output in `dist\`).

## Installing a release

```powershell
$env:TERMINAL_WINDOWS_REPO = "Aneesh-Madupalli/terminal-windows"
irm https://raw.githubusercontent.com/Aneesh-Madupalli/terminal-windows/main/packaging/get.ps1 | iex
```

`get.ps1` downloads the zip, checks its SHA-256 against the published file, and runs `install.ps1`, which installs for the current user without administrator rights.

## Signing

Release binaries are not code-signed yet, so Windows SmartScreen may warn on a downloaded copy. A signing step needs an Authenticode certificate stored as a repository secret; add it to `release.yml` before the packaging step and sign `terminal-windows.exe` and `twagent.exe`. The guest agent verifies updates by SHA-256 over the authenticated channel, so signing is for the host download, not for the guest update.
