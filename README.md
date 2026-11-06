<p align="center"><img src="docs/assets/banner.svg" alt="terminal-windows: a Windows desktop inside your terminal" width="900"></p>

<p align="center">
  <a href="https://github.com/Aneesh-Madupalli/terminal-windows/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/Aneesh-Madupalli/terminal-windows/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="Language: Rust" src="https://img.shields.io/badge/language-Rust-orange.svg">
  <img alt="Platform: Windows" src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078d4.svg">
  <img alt="Backends: Hyper-V and VirtualBox" src="https://img.shields.io/badge/backends-Hyper--V%20%7C%20VirtualBox-7aa2f7.svg">
</p>

# terminal-windows

A real Windows PC in a virtual machine (Hyper-V or VirtualBox) that you, and your AI agents, can see and control from a terminal. Everything happens inside the VM; your own desktop is never touched.

- `terminal-windows up --iso PATH` installs Windows unattended and gets it ready.
- `terminal-windows view` shows the guest desktop in your terminal (Sixel in Windows Terminal, half blocks anywhere) and forwards keyboard and mouse.
- `terminal-windows mcp` serves the guest to an AI agent over MCP: `screenshot`, `click`, `move`, `drag`, `scroll`, `type`, `key`, `status`.
- No shell-execution action exists. The channel between host and guest is a Hyper-V socket, mutually authenticated with a per-VM secret.

## How it fits together

```mermaid
flowchart LR
  subgraph Host["Your PC"]
    CLI["terminal-windows<br/>view | mcp | up | action"]
    Agent["AI agent<br/>(Claude, ...)"]
    CLI --- HV{{"Hyper-V or VirtualBox"}}
  end
  subgraph VM["Windows guest VM"]
    TW["twagent.exe<br/>screen capture + input"]
    Desk["Windows desktop"]
    TW --- Desk
  end
  Agent -- "MCP (stdio)" --> CLI
  CLI == "authenticated channel<br/>Hyper-V socket or 127.0.0.1 forward" ==> TW
  HV -. "creates and controls" .-> VM
```

## What `up` does

```mermaid
flowchart TD
  A["doctor: pick a backend<br/>Hyper-V if ready, else VirtualBox"] --> B["Check the Windows ISO<br/>choose the edition"]
  B --> C["Create the VM, secret and guest password"]
  C --> D["Build the answer ISO<br/>(unattended setup + agent)"]
  D --> E["Boot from the ISO<br/>boot key sent by the host"]
  E --> F["Windows installs itself<br/>and signs in automatically"]
  F --> G["Agent starts at sign-in"]
  G --> H{"Agent answers<br/>session = ready?"}
  H -- no --> H
  H -- yes --> I["Eject media, delete answer ISO<br/>VM is ready"]
```

## A request, step by step

```mermaid
sequenceDiagram
  participant A as AI agent
  participant H as terminal-windows (host)
  participant G as twagent (guest)
  A->>H: tools/call click {x, y}
  H->>G: connect, mutual HMAC handshake
  G-->>H: authenticated
  H->>G: click x, y
  G->>G: SendInput
  G-->>H: ok
  A->>H: tools/call screenshot
  H->>G: screenshot
  G-->>H: PNG
  H-->>A: image
```

## Requirements

- Windows 10/11 **Pro, Enterprise or Education** with Hyper-V, **or any Windows edition (including Home) with [VirtualBox 7](https://www.virtualbox.org)**. `terminal-windows up` uses Hyper-V when it is ready and VirtualBox otherwise; force one with `--backend hyperv|virtualbox`.
- Hardware virtualization on, 8 GiB or more of RAM, 64 GiB of disk.
- A Windows 10 22H2 or Windows 11 x64 ISO.

## Install

```powershell
.\scripts\build.ps1            # builds dist\terminal-windows-<version>-x64.zip
.\scripts\install.ps1          # installs for the current user and adds it to PATH
terminal-windows doctor        # checks this PC
terminal-windows setup-host --enable-hyper-v   # once, from an elevated prompt, if doctor asks
terminal-windows up --iso C:\ISOs\Win11.iso --edition "Windows 11 Pro"
terminal-windows view
```

## Commands

Run `terminal-windows help`. The main ones: `doctor`, `setup-host`, `iso check`, `up`, `view`, `mcp`, `action`, `screenshot`, `logs`, `ls`, `status`, `start`, `stop`, `pause`, `resume`, `show`, `hide`, `checkpoint`, `credential show`, `agent update`, `rm`.

Exit codes: 0 success, 1 failure, 2 usage, 3 prerequisite missing, 4 no such VM, 5 VM not running, 6 guest not ready, 7 access denied, 8 guest locked.

## For AI agents

Add the MCP server to your client (the Claude plugin manifest is in `.claude-plugin`):

```json
{
  "mcpServers": {
    "terminal-windows": { "command": "terminal-windows", "args": ["mcp"] }
  }
}
```

`skills\terminal-windows\SKILL.md` teaches an agent how to drive the guest.

## Layout

| Crate                      | Purpose                                                                            |
| -------------------------- | ---------------------------------------------------------------------------------- |
| `tw-core`                  | Names, arguments, key table, machine records, secret storage                       |
| `tw-proto`, `tw-transport` | Wire protocol, authentication, framing, Hyper-V sockets                            |
| `tw-client`, `tw-mcp`      | Host client, frame stream, MCP server                                              |
| `tw-hv`                    | Hyper-V (PowerShell) and VirtualBox (`VBoxManage`) backends, plus a fake for tests |
| `tw-provision`             | ISO inspection, answer file, answer ISO, credentials                               |
| `tw-agent`                 | Guest agent (`twagent.exe`)                                                        |
| `tw-term`, `tw-viewer`     | Terminal graphics, input and the viewer                                            |
| `tw-cli`                   | The `terminal-windows` command                                                     |

Design documents: `docs\PRD.md`, `docs\TDD.md`, `docs\VALIDATION.md`.

## Status

Every crate is built and tested without a hypervisor (about 320 tests, including real PowerShell, DPAPI, ISO building and mounting, and screen capture on the build machine). The Hyper-V and VirtualBox backends, the guest agent's input injection and the unattended install have not yet run against a real hypervisor (the VirtualBox backend is tested against a simulator of `VBoxManage`); `docs\VALIDATION.md` lists the checks to run there first.

## Development

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

MIT licensed. Third-party notices: `THIRD-PARTY-NOTICES.md`.
