# Validation

This file records what has actually been verified. Nothing below is verified until a result is filled in. Do not describe a behaviour as working before its row has a date, an environment and an outcome.

## Environment record (fill in per run)

| Field                            | Value |
| -------------------------------- | ----- |
| Date                             |       |
| Host Windows version and edition |       |
| Host CPU, RAM                    |       |
| Hyper-V enabled                  |       |
| Guest OS and build               |       |
| Terminal and version             |       |
| Commit                           |       |

## Phase 0 spikes

Questions, experiments and fallbacks are defined in [TDD.md section 19](TDD.md#19-risks-and-phase-0-validation-spikes).

| Spike | Question                                                                                                                                     | Result (pass, fail, partial) | Measurements and notes                                | Decision taken |
| ----- | -------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------- | ----------------------------------------------------- | -------------- |
| S1    | Desktop Duplication in a Gen 2 Hyper-V guest                                                                                                 |                              | frames per second, CPU, errors seen                   |                |
| S2    | Guest resolutions, 1280×800, pointer accuracy, scroll signs                                                                                  |                              | resolutions tested, pixel error, wheel results        |                |
| S3    | Enhanced Session Mode versus the agent's session                                                                                             |                              | session IDs before and after `vmconnect`              |                |
| S4    | Windows Setup finds `autounattend.xml` on the answer ISO; editions needing `BypassNRO`; first-logon elevation; non-elevated ISO and WIM read |                              | ISO builder used, editions tried                      |                |
| S5    | Host-initiated hvsock to a guest listener, registration, Winsock error table, Tokio support                                                  |                              | error code per VM state, registration required or not |                |
| S6    | Terminal capabilities, Kitty z-index and delete, Sixel throughput, `Msvm_Keyboard`, resize and VT input under ConPTY                         |                              | replies per terminal, usable fps                      |                |
| S7    | Storage ACLs under the user data directory; checkpoint restore with vTPM and hvsock reconnect                                                |                              | restore result, reconnect time                        |                |

## Automated checks

| Check                                                  | Date | Result |
| ------------------------------------------------------ | ---- | ------ |
| `cargo fmt --check`                                    |      |        |
| `cargo clippy -D warnings`                             |      |        |
| Unit and property tests (Windows)                      |      |        |
| Unit and property tests (Linux, OS-independent crates) |      |        |
| Package check (`scripts/test-package.ps1`)             |      |        |

## Live acceptance checklist (P0)

Mark each item with the date, environment and result. Items map to [TDD.md section 17.3](TDD.md#173-acceptance-checklist).

- [ ] `doctor` is correct with Hyper-V enabled, disabled, and without rights
- [ ] `setup-host` is idempotent
- [ ] `up` from an ISO reaches a ready desktop with zero guest interaction; a repeated `up` reuses the VM
- [ ] Interrupted install, then `up` again
- [ ] `action status`, screenshot content, pointer accuracy at 100 percent and 150 percent guest DPI scale
- [ ] Agent types and clicks in the guest while the host's focus, pointer and clipboard stay unchanged
- [ ] Viewer in Windows Terminal and WezTerm: resize, Unicode, arrows, click, drag, scroll, palette, `Ctrl+]`, closing the terminal window, `Ctrl+C` and `Ctrl+Break`
- [ ] Pause and resume; graceful and forced stop; fresh boot after a forced stop
- [ ] Guest has no shared folders; `--offline` VM has no network adapter
- [ ] Lock screen, sign-out and UAC: states are reported correctly and nothing crashes
- [ ] Agent update: push a newer build, verify restart and rollback
- [ ] MCP client completes a multi-step task (open an app with `win`, type, screenshot, verify)

## Known limitations observed

(Record anything found during validation that users must be told about.)
