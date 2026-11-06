# terminal-windows — Product Requirements Document (PRD)

| Field                   | Value                                                                                   |
| ----------------------- | --------------------------------------------------------------------------------------- |
| Product                 | `terminal-windows`                                                                      |
| Document                | Product Requirements Document                                                           |
| Status                  | Draft 1 (for review)                                                                    |
| Date                    | 2026-11-06                                                                              |
| Reference product       | [`terminal-mac`](../../terminal-mac) (cloned at `e:\experiments\terminal\terminal-mac`) |
| Companion document      | [TDD.md](TDD.md) (Technical Design Document)                                            |
| Implementation language | Rust (stable, edition 2024)                                                             |

> **Provisional decisions.** The hypervisor choice (Hyper-V first, QEMU + WHPX second) and the guest OS choice (Windows first) are provisional, because they were not explicitly confirmed. Both are recorded in [section 16](#16-open-questions-and-decisions) and can be changed before implementation starts.

---

## Table of contents

1. [Summary](#1-summary)
2. [Background and reference product](#2-background-and-reference-product)
3. [Problem statement](#3-problem-statement)
4. [Goals and non-goals](#4-goals-and-non-goals)
5. [Users and personas](#5-users-and-personas)
6. [Platform scope and the meaning of "parity"](#6-platform-scope-and-the-meaning-of-parity)
7. [User journeys](#7-user-journeys)
8. [Functional requirements](#8-functional-requirements)
9. [Non-functional requirements](#9-non-functional-requirements)
10. [User experience specification](#10-user-experience-specification)
11. [Security and isolation model (product view)](#11-security-and-isolation-model-product-view)
12. [Constraints, assumptions and dependencies](#12-constraints-assumptions-and-dependencies)
13. [Risks](#13-risks)
14. [Success metrics](#14-success-metrics)
15. [Release plan](#15-release-plan)
16. [Open questions and decisions](#16-open-questions-and-decisions)
17. [Glossary](#17-glossary)
18. [Appendix A: capability parity matrix](#appendix-a-capability-parity-matrix)
19. [Appendix B: reference facts taken from terminal-mac](#appendix-b-reference-facts-taken-from-terminal-mac)

---

## 1. Summary

`terminal-windows` gives computer-use agents (and people) a **separate Windows desktop** that is viewed and controlled from a terminal. A real Windows guest runs inside a Hyper-V virtual machine on the user's Windows PC. A small helper program **inside the guest** captures the guest screen and injects keyboard and mouse input. The host never uses its own screen or input APIs to act on behalf of an agent, so the user's physical desktop, pointer and clipboard are untouched.

Three interfaces sit on top of the same guest control channel:

1. **A terminal viewer** that streams the guest desktop into a graphics-capable terminal and forwards typing, mouse, scroll and shortcuts.
2. **A command line** (`terminal-windows action ...`) that prints JSON and works in scripts.
3. **An MCP server** (`terminal-windows mcp`) that exposes `screenshot`, `click`, `move`, `drag`, `scroll`, `type`, `key` and `status` to any MCP-capable agent, together with a skill file that teaches the observe, act, verify loop.

The product is a Windows-host re-implementation of the idea behind `terminal-mac`, written in Rust, with the same user-visible shape (CLI verbs, MCP tools, viewer behaviour, isolation story) and the improvements that Windows and Hyper-V make possible (fully unattended guest setup, cheap checkpoints, a push-based frame stream, no per-app privacy prompts inside the guest).

## 2. Background and reference product

### 2.1 What `terminal-mac` is

`terminal-mac` is a Swift command-line tool for Apple silicon Macs. Its capabilities, as read from its source:

| Area            | What it does                                                                                                                                                                                                                                                                                                                                                                  |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| VM              | Uses Apple's Virtualization framework to install and run a real **macOS** guest from an Apple restore image (`.ipsw`). VM data lives under `~/Library/Application Support/terminal-mac/machines/NAME/`.                                                                                                                                                                       |
| Guest helper    | `TerminalMacGuest.app` runs inside the guest. It captures the screen with ScreenCaptureKit and posts input with `CGEvent`. It refuses to run unless `hw.model` starts with `VirtualMac`. It installs itself in the guest, registers as a login item and updates itself from a shared folder.                                                                                  |
| Transport       | A flat file **mailbox** on a virtio-fs shared folder. The host writes `request-<uuid>.json`, the guest writes `reply-<uuid>.json`. No network listener. Symlinks, hard links and oversized files are rejected.                                                                                                                                                                |
| Viewer          | Streams the desktop into Ghostty, Kitty or iTerm2 using the Kitty or iTerm2 graphics protocols. Frames are sent only when the screen changes (up to 30 fps), as lz4-compressed pixels. Supports typing, clicks, double and triple clicks, drags, scroll, arrows and control keys. `Ctrl+G` opens a Mac-shortcut palette, `Ctrl+O` opens the native VM window, `Ctrl+]` exits. |
| Agent interface | An MCP server over stdio with eight tools, and a bundled skill (`SKILL.md`).                                                                                                                                                                                                                                                                                                  |
| CLI             | `up`, `setup`, `image`, `create`, `ls`, `start`, `status`, `show`, `hide`, `pause`, `resume`, `stop`, `display`, `action`, `view`, `mcp`, `skills`, `doctor`, `guest`.                                                                                                                                                                                                        |
| Setup           | Guided but **not unattended**: the user completes macOS Setup Assistant, opens the helper once, and grants Screen Recording and Accessibility inside the guest.                                                                                                                                                                                                               |
| Packaging       | Self-signed `.app`, `install.sh`, a checksum-pinned `get.sh` curl installer, optional notarization.                                                                                                                                                                                                                                                                           |

### 2.2 What does not carry over

- **The guest OS.** The repository's central capability is a _macOS_ guest. macOS can legally run virtualized only on Apple hardware, so it cannot be reproduced on Windows hosts. `terminal-windows` therefore uses a _Windows_ guest.
- **The hypervisor API.** Apple's Virtualization framework does not exist on Windows. The equivalent is Hyper-V (primary) and, later, QEMU with the Windows Hypervisor Platform (WHPX).
- **The guest helper.** ScreenCaptureKit, `CGEvent`, TCC permissions and `SMAppService` have Windows counterparts (Desktop Duplication or GDI, `SendInput`, no per-app prompts, a scheduled task).
- **The transport.** virtio-fs shares are replaced by Hyper-V sockets (`AF_HYPERV`), a host-to-guest byte stream that needs no network adapter.

### 2.3 What carries over

The request and reply protocol shape, the CLI verbs, the MCP tool surface and semantics, the skill, the viewer's layout and input model, the isolation principles (no network listener, bounded messages, no shell-execution action) and the packaging approach.

## 3. Problem statement

Agents that operate GUIs need a place to do it. Running them against the user's real desktop is risky and disruptive: an agent can click the wrong thing, steal focus, type into the user's real windows, or touch real accounts and files. Users on Windows today have no lightweight, terminal-native way to give an agent its own full Windows desktop that:

- starts with one command and needs almost no manual setup,
- can be watched and taken over from a terminal when needed,
- is controlled through a small, stable tool surface (MCP, CLI),
- is separate from the host's apps, files, accounts, pointer and clipboard,
- can be reset quickly after an agent makes a mess.

## 4. Goals and non-goals

### 4.1 Goals

| ID  | Goal                                                                                                                                                                                          |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| G1  | Provide an isolated Windows desktop that agents control through MCP and CLI, without touching the host's desktop, pointer or clipboard.                                                       |
| G2  | Provide a live terminal viewer with keyboard, mouse and scroll control, comparable to `terminal-mac view`.                                                                                    |
| G3  | Reach a working desktop with **one command** (`terminal-windows up`) after a one-time elevated host setup. Guest installation, account creation, agent installation and login are unattended. |
| G4  | Keep the same user-visible shape as `terminal-mac` so existing agent skills and mental models transfer (same MCP tool names and argument shapes, same CLI verbs where the concept exists).    |
| G5  | Make reset cheap: named checkpoints and restore, so an agent run can be rolled back.                                                                                                          |
| G6  | Be safe by default: no network listener, authenticated control channel, bounded inputs, explicit guest/host separation.                                                                       |
| G7  | Be written in Rust as a small set of crates with a testable, OS-independent core and a hypervisor abstraction that allows additional backends later.                                          |
| G8  | Ship as a signed, checksum-verified, no-compiler-needed installer.                                                                                                                            |

### 4.2 Non-goals (version 1)

| ID  | Non-goal                                                                                      | Reason                                                                                                                          |
| --- | --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| NG1 | macOS guests                                                                                  | Not permitted or technically supported off Apple hardware.                                                                      |
| NG2 | Linux guests                                                                                  | Planned later through the backend and guest-agent abstractions, not in v1.                                                      |
| NG3 | Linux or macOS **hosts**                                                                      | v1 is Windows-host only. The design keeps OS-specific code behind traits so other hosts are possible.                           |
| NG4 | Defending against a hostile guest or a malicious administrator on the host                    | The product is an isolation convenience for agents, not a hardened malware sandbox. NAT networking can reach the local network. |
| NG5 | GPU passthrough, audio, USB, clipboard sharing, drag and drop of files between host and guest | Not needed for the agent loop.                                                                                                  |
| NG6 | Remote (over the network) control or multi-user access                                        | The control channel is host-local by design.                                                                                    |
| NG7 | Distributing Windows images or product keys                                                   | The user supplies a licensed or evaluation ISO.                                                                                 |
| NG8 | An accessibility-tree "snapshot"                                                              | `snapshot` stays an alias of `screenshot`, as in `terminal-mac`. May be added later using UI Automation.                        |
| NG9 | Windows Home support through Hyper-V                                                          | Hyper-V is not available on Home. Home support is delivered by the QEMU + WHPX backend (P1, section 15).                        |

## 5. Users and personas

| Persona                       | Needs                                                                                                       | Primary interface                         |
| ----------------------------- | ----------------------------------------------------------------------------------------------------------- | ----------------------------------------- |
| **Agent developer**           | A reproducible Windows GUI environment for an agent to test Windows apps and websites. Resets between runs. | MCP, CLI, checkpoints                     |
| **Agent end user**            | Let an agent do GUI tasks without touching their own desktop. Be able to watch and intervene.               | Terminal viewer, MCP through an assistant |
| **QA or automation engineer** | Scripted screenshots, clicks and typing against a clean Windows install.                                    | CLI JSON output                           |
| **Security-conscious user**   | Know exactly what the guest can reach and what the tool installs.                                           | `doctor`, docs, `--offline`               |

## 6. Platform scope and the meaning of "parity"

### 6.1 Support matrix

| Dimension               | v1                                                                                                 | Later                                                                          |
| ----------------------- | -------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Host OS                 | Windows 11, Windows 10 version 1809 or later, 64-bit, x86-64                                       | Windows on Arm (needs guest and agent builds for arm64), Linux and macOS hosts |
| Host edition            | Pro, Enterprise, Education (Hyper-V available)                                                     | Home through QEMU + WHPX                                                       |
| Hypervisor              | Hyper-V (Generation 2 VMs)                                                                         | QEMU with WHPX, KVM or HVF                                                     |
| Guest OS                | Windows 11 and Windows 10 (user-supplied ISO, evaluation ISOs supported)                           | Linux (X11 or Wayland agent)                                                   |
| Terminal for the viewer | Windows Terminal (Sixel), WezTerm (Kitty, iTerm2, Sixel), any VT terminal with half-block fallback | More protocols as terminals add them                                           |

### 6.2 What "parity" means

"100 percent of the capabilities" is interpreted as **every user-visible capability in `terminal-mac`'s capability inventory has a Windows equivalent**, except where the capability is a property of the macOS guest itself. Appendix A lists every capability with its Windows equivalent, whether it is identical, adapted or improved, and its priority.

Capabilities that are intrinsically macOS and are therefore **replaced, not ported**:

| `terminal-mac` capability                                | Replacement                                                                                                            |
| -------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| macOS restore image download from Apple                  | User-supplied Windows ISO (`--iso`), validated before use.                                                             |
| Screen Recording and Accessibility approval in the guest | Not needed on Windows. The agent only needs to run in the interactive session.                                         |
| Guest Login Items (`SMAppService`)                       | Scheduled task at logon, plus automatic logon of the agent account.                                                    |
| virtio-fs shared folders `control` and `tools`           | Hyper-V socket channel for control. A read-only setup ISO for first install. Push updates over the channel afterwards. |
| ⌘ shortcuts palette                                      | Windows shortcuts palette (`Win`, `Alt+Tab`, `Win+R`, `Ctrl+Alt+Del` and others).                                      |

## 7. User journeys

### 7.1 First run (new user)

1. The user installs `terminal-windows` (one PowerShell line or a ZIP and `install.ps1`).
2. They run `terminal-windows setup-host` once from an elevated prompt. It checks that Hyper-V is available, registers the control-channel service ID, and adds the user to the _Hyper-V Administrators_ group. It tells the user if a sign-out is required.
3. They run `terminal-windows up --iso D:\Win11_Enterprise_Eval.iso`.
4. The tool validates the ISO, shows the plan (disk, memory, CPUs, network), creates the VM, installs Windows **unattended**, installs the guest agent, signs in the agent account automatically, and waits until the agent answers.
5. The viewer opens in the terminal and shows the guest desktop. Total user interaction after the command: none, apart from waiting.

### 7.2 Daily use

1. `terminal-windows` (no arguments in an interactive terminal) runs `up`: starts the VM if it is off, waits for the agent, opens the viewer.
2. The user can type and click in the viewer, press `Ctrl+G` for the shortcut palette, and `Ctrl+]` to leave. The VM keeps running.

### 7.3 Agent use (MCP)

1. The user registers the server once, for example `claude mcp add terminal-windows -- terminal-windows mcp`, or pastes the output of `terminal-windows mcp --config` into another client.
2. The agent calls `status`, then `screenshot`, then acts with `click`, `type`, `key`, `scroll`, `drag`, `move` and verifies with another `screenshot`.
3. If the guest has no usable session (for example at the lock screen after an unexpected sign-out), the tool says so in plain language so the agent can ask the user to intervene.

### 7.4 Reset

1. Before a risky task: `terminal-windows checkpoint create clean`.
2. After: `terminal-windows checkpoint restore clean`. The guest returns to the saved state in seconds, including a running desktop and agent.

### 7.5 Recovery

| Situation                               | Expected behaviour                                                                                                      |
| --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| Hyper-V disabled                        | `doctor` and `up` explain exactly which Windows feature to enable and how, and exit with code 3.                        |
| Not in the Hyper-V Administrators group | The command explains the group, how to add the user (or runs `setup-host` elevated), and exits with code 7.             |
| Interrupted installation                | VM stays registered with `installed: false`. `up` resumes or restarts the install; it never silently overwrites disks.  |
| Agent not responding                    | The viewer shows "guest helper is not responding" and the next action to try. The CLI exits with code 6 and a message.  |
| Guest at a lock or sign-in screen       | `status` reports `session: locked` or `session: no-session`. The viewer offers host-side typing for the sign-in screen. |

## 8. Functional requirements

Priority: **P0** is required for the first release. **P1** follows within the first minor releases. **P2** is later.

### 8.1 Host prerequisites and diagnostics

| ID        | Requirement                                                                                                                                                                                                                                                                                                                                      | Pri |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --- |
| FR-HOST-1 | `doctor` reports: Windows version and edition, CPU virtualization enabled in firmware, Hyper-V feature state, Hyper-V Management service state, current user's rights (elevated or member of Hyper-V Administrators), service registration state, terminal graphics capability, data directory, and bundled guest agent version. Output is JSON. | P0  |
| FR-HOST-2 | `setup-host` performs the one-time elevated steps (service registration, group membership) and refuses to enable Windows features without an explicit `--enable-hyper-v` flag.                                                                                                                                                                   | P0  |
| FR-HOST-3 | Every host precondition failure has a distinct message and a distinct exit code (section 10.4).                                                                                                                                                                                                                                                  | P0  |
| FR-HOST-4 | `TERMINAL_WINDOWS_HOME` overrides the data root.                                                                                                                                                                                                                                                                                                 | P0  |

### 8.2 VM lifecycle

| ID      | Requirement                                                                                                                                                                                                                                                                                         | Pri |
| ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| FR-VM-1 | `create NAME --iso PATH` creates a Generation 2 Hyper-V VM with Secure Boot and a virtual TPM, a dynamically expanding VHDX, and the chosen CPU, memory, disk and display size.                                                                                                                     | P0  |
| FR-VM-2 | Defaults: min(4, host logical processors ÷ 2) vCPUs but at least 2, 8 GiB memory (never more than half of the host's RAM), 64 GiB dynamic disk, 1280×800 display, NAT network. Ranges: CPUs 2 to the host maximum, memory 4 to 128 GiB, disk 64 to 2048 GiB, width 800 to 1920, height 600 to 1200. | P0  |
| FR-VM-3 | VM names match `^[a-zA-Z0-9][a-zA-Z0-9_-]{0,47}$`. Hyper-V names are namespaced as `tw-NAME` so the tool never touches unrelated VMs.                                                                                                                                                               | P0  |
| FR-VM-4 | `start`, `stop [--force]`, `pause`, `resume`, `status`, `ls` behave as in `terminal-mac`. `stop` requests a normal guest shutdown; `--force` powers off immediately and warns about data loss.                                                                                                      | P0  |
| FR-VM-5 | `--offline` creates the VM without a network adapter. The control channel still works.                                                                                                                                                                                                              | P0  |
| FR-VM-6 | A VM is never reinstalled or overwritten by `up` or `create`.                                                                                                                                                                                                                                       | P0  |
| FR-VM-7 | `display NAME --size WxH` changes the guest display size while the VM is stopped.                                                                                                                                                                                                                   | P1  |
| FR-VM-8 | `rm NAME --yes` deletes the VM, its disks and its metadata after explicit confirmation. (`terminal-mac` deliberately has no delete; Hyper-V makes safe deletion practical.)                                                                                                                         | P1  |
| FR-VM-9 | Concurrent mutating operations on the same VM are serialized with a lock and fail with a clear message instead of corrupting state.                                                                                                                                                                 | P0  |

### 8.3 Unattended guest installation

| ID        | Requirement                                                                                                                                                                                                                                                            | Pri |
| --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| FR-INST-1 | `create` and `up` accept `--iso PATH`. The ISO is validated (readable, contains a Windows installation image, architecture matches the host, an image index or edition can be selected) before any disk is created. Reading the ISO normally needs no elevated prompt. | P0  |
| FR-INST-2 | Installation is unattended: disk partitioning, image selection, local administrator account creation (random password), automatic logon, skipping of first-run screens, and installation of the guest agent. The user is never asked to touch the guest.               | P0  |
| FR-INST-3 | The tool presses the "Press any key to boot from CD or DVD" key on the VM's behalf.                                                                                                                                                                                    | P0  |
| FR-INST-4 | The generated administrator password is stored on the host in Windows Credential Manager (DPAPI-protected), never printed by default, and shown only by an explicit `credential show NAME` command.                                                                    | P0  |
| FR-INST-5 | The edition is selected with `--edition "Windows 11 Pro"` or `--image-index N`. If omitted and the ISO contains exactly one edition, that one is used; if several, the command lists them and fails.                                                                   | P0  |
| FR-INST-6 | Installation progress is reported as phases (`creating`, `installing`, `provisioning`, `waiting-for-agent`, `ready`) with elapsed time.                                                                                                                                | P0  |
| FR-INST-7 | The tool never downloads Windows. `iso check PATH` explains where evaluation ISOs can be obtained.                                                                                                                                                                     | P0  |
| FR-INST-8 | A product key may be supplied with `--product-key` (stored only in the answer media for that install).                                                                                                                                                                 | P2  |

### 8.4 Guest agent

| ID         | Requirement                                                                                                                                                                                                                            | Pri |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| FR-AGENT-1 | A single Rust executable inside the guest listens on a Hyper-V socket, authenticates the host, and serves the actions in 8.5. It runs in the interactive user session at logon with high integrity (so it can drive elevated windows). | P0  |
| FR-AGENT-2 | The agent refuses to run unless it detects a Hyper-V guest (and optionally a tool-created VM marker). It never controls a physical machine.                                                                                            | P0  |
| FR-AGENT-3 | The agent reports its state: `ready`, `no-session`, `locked`, `secure-desktop`, `capture-error`, with details.                                                                                                                         | P0  |
| FR-AGENT-4 | The agent starts capturing only when a viewer subscribes and stops when the subscription ends. An idle desktop costs nothing.                                                                                                          | P0  |
| FR-AGENT-5 | The agent updates itself: the host pushes a newer signed build over the channel; the agent verifies its hash, installs it and restarts.                                                                                                | P1  |
| FR-AGENT-6 | The agent logs to a rotating file in the guest and can return its log tail on request.                                                                                                                                                 | P1  |

### 8.5 Agent and viewer actions

| ID        | Action                     | Requirement                                                                                                                                                                                                                                    | Pri |
| --------- | -------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| FR-ACT-1  | `status`                   | Returns VM state, session state, agent version, display width, height, DPI scale.                                                                                                                                                              | P0  |
| FR-ACT-2  | `screenshot`               | Returns a PNG (CLI) or JPEG (MCP) at guest pixel size, with coordinates equal to image pixels.                                                                                                                                                 | P0  |
| FR-ACT-3  | `click`                    | Left or right button; `double` and `clicks` 1 to 3; pixel coordinates; bounds checked.                                                                                                                                                         | P0  |
| FR-ACT-4  | `move`                     | Moves the pointer without clicking (hover menus, tooltips).                                                                                                                                                                                    | P0  |
| FR-ACT-5  | `drag`                     | Press at (x, y), move in interpolated steps, release at (x2, y2).                                                                                                                                                                              | P0  |
| FR-ACT-6  | `scroll`                   | Vertical and horizontal, range -1000 to 1000. Positive `dy` is a wheel turned away from the user (the view scrolls up); positive `dx` is a wheel pushed left (the view scrolls left). Same convention as `terminal-mac`; verified in spike S2. | P0  |
| FR-ACT-7  | `type`                     | Literal Unicode text up to 32 KiB, sent as Unicode key events without using the clipboard.                                                                                                                                                     | P0  |
| FR-ACT-8  | `key`                      | A key or chord such as `return`, `tab`, `escape`, `win+r`, `ctrl+c`, `alt+tab`, `ctrl+shift+esc`, function keys, arrows, navigation keys. Modifiers: `ctrl`, `alt`, `shift`, `win`.                                                            | P0  |
| FR-ACT-9  | `ctrl+alt+del`             | Sent from the host through the hypervisor keyboard device because the guest agent cannot inject the secure attention sequence.                                                                                                                 | P1  |
| FR-ACT-10 | Host-side typing           | While the guest is at a lock, sign-in or secure desktop, the viewer can type through the hypervisor keyboard device.                                                                                                                           | P1  |
| FR-ACT-11 | `screenshot` after actions | MCP tools accept `screenshot: true` to return an image taken about 400 ms after the action.                                                                                                                                                    | P0  |

### 8.6 Terminal viewer

| ID         | Requirement                                                                                                                                                                                                                                                                 | Pri |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| FR-VIEW-1  | `view [NAME]` streams the guest desktop into the terminal, aspect-correct, with a header (brand, VM name, display size, link state, fps, latency), a rounded frame and a footer (key hints and last action).                                                                | P0  |
| FR-VIEW-2  | Graphics protocols: Kitty (PNG), iTerm2 inline images and Sixel, chosen by detection; `--protocol kitty\|iterm\|sixel\|blocks` overrides.                                                                                                                                   | P0  |
| FR-VIEW-3  | A universal fallback renders the desktop with Unicode half-blocks and truecolor for terminals with no graphics protocol. Lower fidelity, view and basic click support.                                                                                                      | P1  |
| FR-VIEW-4  | Frames are pushed by the guest only when the screen changes, at up to 30 fps, already scaled to the size the terminal draws. Slow terminals drop frames instead of queueing them.                                                                                           | P0  |
| FR-VIEW-5  | Input: typed text, arrows, navigation keys, function keys where the terminal reports them, control keys, left and right click, double and triple click, drag (sent as one drag on release), wheel scroll (vertical and horizontal), merged typing and merged scroll events. | P0  |
| FR-VIEW-6  | `Ctrl+G` opens a Windows shortcut palette (Start, Task View, Alt+Tab, Show desktop, Run, Close window, Task Manager, Settings, Copy, Paste, Cut, Select all, Undo, Find, Ctrl+Alt+Del). A single key picks an entry, `Esc` closes.                                          | P0  |
| FR-VIEW-7  | `Ctrl+]` exits the viewer. The VM keeps running. Terminal state is always restored, including on `SIGINT`-equivalent console events and when the terminal closes.                                                                                                           | P0  |
| FR-VIEW-8  | `Ctrl+O` opens the native VM window (`vmconnect`) for sign-in or troubleshooting and hides it again when the guest is ready.                                                                                                                                                | P1  |
| FR-VIEW-9  | `--read-only` shows the desktop but sends no input.                                                                                                                                                                                                                         | P0  |
| FR-VIEW-10 | Link states are shown clearly: connecting, live (with fps), waiting for guest, at lock or sign-in screen, agent not responding.                                                                                                                                             | P0  |
| FR-VIEW-11 | Viewer works inside Windows Terminal and WezTerm without requiring configuration.                                                                                                                                                                                           | P0  |

### 8.7 CLI

| ID       | Requirement                                                                                                                                                                                                                                                  | Pri                                                             |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------- |
| FR-CLI-1 | Every command prints JSON and returns a nonzero exit status on failure. Human progress goes to stderr.                                                                                                                                                       | P0                                                              |
| FR-CLI-2 | Verbs: `up`, `setup`, `setup-host`, `doctor`, `iso check`, `create`, `ls`, `start`, `stop`, `status`, `pause`, `resume`, `show`, `hide`, `display`, `view`, `action`, `mcp`, `skills`, `checkpoint`, `credential`, `rm`, `logs`, `agent`, `version`, `help`. | P0 (core) / P1 (`display`, `rm`, `checkpoint`, `logs`, `agent`) |
| FR-CLI-3 | `terminal-windows` with no arguments in an interactive terminal runs `up`.                                                                                                                                                                                   | P0                                                              |
| FR-CLI-4 | `up --plan` prints the planned steps without changing anything or reading the network. `up --no-view` waits for readiness and prints JSON. `up --detach` starts without waiting.                                                                             | P0                                                              |
| FR-CLI-5 | Argument parsing rejects unknown and duplicate options and validates ranges before any long operation starts.                                                                                                                                                | P0                                                              |

### 8.8 MCP server

| ID       | Requirement                                                                                                                                                                                                                          | Pri |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --- |
| FR-MCP-1 | `terminal-windows mcp [NAME]` speaks MCP over stdio (JSON-RPC, one message per line) and implements `initialize`, `ping`, `tools/list`, `tools/call`.                                                                                | P0  |
| FR-MCP-2 | Tools: `screenshot`, `click`, `move`, `drag`, `scroll`, `type`, `key`, `status`, with the same argument names and semantics as `terminal-mac` (`x`, `y`, `button`, `clicks`, `toX`, `toY`, `dx`, `dy`, `text`, `key`, `screenshot`). | P0  |
| FR-MCP-3 | The server starts and lists tools even when no VM exists. Tool calls fail with an instruction the agent can relay to the user.                                                                                                       | P0  |
| FR-MCP-4 | `mcp --config` prints an `mcpServers` entry pointing at the stable launcher path.                                                                                                                                                    | P0  |
| FR-MCP-5 | Errors are returned as tool results with `isError: true` and plain-language guidance.                                                                                                                                                | P0  |

### 8.9 Skill and agent integration

| ID         | Requirement                                                                                                                                                                            | Pri |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| FR-SKILL-1 | A `SKILL.md` for `terminal-windows` is bundled and installable (`skills install [--dir PATH]`, `skills get`, `skills path`). It never overwrites a different skill with the same name. | P0  |
| FR-SKILL-2 | The skill documents the observe, act, verify loop, Windows key names, when to stop and ask the user, and safety rules.                                                                 | P0  |
| FR-SKILL-3 | A Claude Code plugin manifest and marketplace entry are provided.                                                                                                                      | P1  |

### 8.10 Checkpoints (new relative to `terminal-mac`)

| ID        | Requirement                                                                                                                                                                                                                     | Pri |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| FR-CKPT-1 | `checkpoint create\|ls\|restore\|rm NAME LABEL` map to Hyper-V Standard checkpoints (including memory and device state). The command is named `checkpoint` to avoid confusion with the `snapshot` action alias of `screenshot`. | P1  |
| FR-CKPT-2 | Automatic Hyper-V checkpoints are disabled on tool-created VMs so they do not appear unexpectedly.                                                                                                                              | P0  |
| FR-CKPT-3 | After `checkpoint restore`, the viewer and MCP tools recover without restarting.                                                                                                                                                | P1  |

### 8.11 Packaging and updates

| ID       | Requirement                                                                                                                                             | Pri |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| FR-PKG-1 | A ZIP release containing `terminal-windows.exe`, the guest agent, the skill and `install.ps1`, plus a SHA-256 file.                                     | P0  |
| FR-PKG-2 | `install.ps1` installs to `%LOCALAPPDATA%\Programs\terminal-windows` and a launcher on the user PATH without administrator rights, verifying checksums. | P0  |
| FR-PKG-3 | A one-line installer script (`irm .../get.ps1 \| iex`) pinned to a release checksum.                                                                    | P1  |
| FR-PKG-4 | Binaries are Authenticode-signed.                                                                                                                       | P1  |
| FR-PKG-5 | A `winget` manifest.                                                                                                                                    | P2  |
| FR-PKG-6 | Releases include a `THIRD-PARTY-NOTICES.txt` listing the licenses of all bundled Rust crates.                                                           | P0  |

## 9. Non-functional requirements

### 9.1 Performance targets

These are design targets to be verified by the acceptance tests, not measurements.

| ID         | Target                                                                                                                                              |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| NFR-PERF-1 | Input action round trip (host command to guest event posted and reply received), median under 30 ms, 95th percentile under 100 ms.                  |
| NFR-PERF-2 | A single `screenshot` (capture, encode, transfer, decode on host) median under 250 ms at 1280×800.                                                  |
| NFR-PERF-3 | Live view up to 30 fps for a changing desktop at the size the terminal draws; Sixel mode is allowed to cap lower (see TDD section 11).              |
| NFR-PERF-4 | Idle CPU use on the host under 1 percent when a viewer is attached to an unchanged desktop. No capture work in the guest when nobody is subscribed. |
| NFR-PERF-5 | Time from `up` on an existing stopped VM to a visible desktop under 60 seconds on a typical SSD machine.                                            |
| NFR-PERF-6 | First-time unattended install: dominated by Windows setup time; the tool adds under 2 minutes of its own overhead.                                  |

### 9.2 Reliability

| ID        | Requirement                                                                                                                                                                                               |
| --------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| NFR-REL-1 | A crashed viewer, CLI or MCP server never affects the VM. VM state is owned by Hyper-V, not by a tool process.                                                                                            |
| NFR-REL-2 | Every wait has a deadline and a clear timeout message. No operation can hang forever.                                                                                                                     |
| NFR-REL-3 | The terminal is always restored on exit, including on panic, console close and Ctrl+C/Break events.                                                                                                       |
| NFR-REL-4 | The agent survives guest sleep prevention, screen saver and idle lock policies because the tool disables them during provisioning.                                                                        |
| NFR-REL-5 | Re-running any command is safe (idempotent where it makes sense).                                                                                                                                         |
| NFR-REL-6 | Windows Update must not reboot the guest while the agent is in use: provisioning sets the policy that prevents automatic restarts while a user is signed in. Updates themselves stay at Windows defaults. |

### 9.3 Security

| ID        | Requirement                                                                                                                             |
| --------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| NFR-SEC-1 | No TCP, UDP or named-pipe **network** listener is opened on the host or in the guest for control.                                       |
| NFR-SEC-2 | The control channel is authenticated (per-VM secret, challenge and response) even though Hyper-V sockets are host-local.                |
| NFR-SEC-3 | All messages and files are size-bounded and strictly validated; invalid input is rejected, never interpreted.                           |
| NFR-SEC-4 | There is no "run command" or file-transfer action in the control protocol (parity with `terminal-mac`).                                 |
| NFR-SEC-5 | Secrets (administrator password, channel secret) are never written to logs and are stored with DPAPI or user-only ACLs.                 |
| NFR-SEC-6 | All PowerShell is invoked with fixed scripts; user-controlled values are passed as validated data, never concatenated into script text. |
| NFR-SEC-7 | The guest agent refuses to run on a physical machine.                                                                                   |

### 9.4 Usability

| ID       | Requirement                                                                                    |
| -------- | ---------------------------------------------------------------------------------------------- |
| NFR-UX-1 | Every error says what happened, why, and the next command or action.                           |
| NFR-UX-2 | The tool works with `NO_COLOR` and when output is redirected.                                  |
| NFR-UX-3 | Messages avoid jargon; guest-state words (`locked`, `no-session`) are explained in the viewer. |

### 9.5 Compatibility

| ID         | Requirement                                                                                                                                                                   |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| NFR-COMP-1 | Host: Windows 10 1809 or later and Windows 11, Pro, Enterprise or Education, x86-64.                                                                                          |
| NFR-COMP-2 | Guest: Windows 10 22H2 and Windows 11 (23H2 or later recommended).                                                                                                            |
| NFR-COMP-3 | Terminals: Windows Terminal 1.22 or later for Sixel; WezTerm for Kitty, iTerm2 and Sixel (both to be confirmed by spike S6). Other terminals through the half-block fallback. |
| NFR-COMP-4 | Coexists with other virtualization products that use the Windows Hypervisor Platform.                                                                                         |

### 9.6 Observability

| ID        | Requirement                                                                                   |
| --------- | --------------------------------------------------------------------------------------------- |
| NFR-OBS-1 | Host logs to `%LOCALAPPDATA%\terminal-windows\logs`, with `--verbose` and `RUST_LOG` control. |
| NFR-OBS-2 | Guest agent logs are retrievable with `logs NAME --agent`.                                    |
| NFR-OBS-3 | No telemetry is collected or sent.                                                            |

### 9.7 Maintainability

| ID        | Requirement                                                                                                                                  |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| NFR-MNT-1 | The wire protocol, models, argument parsing, input parsing and layout math live in OS-independent crates with unit tests that run on any OS. |
| NFR-MNT-2 | The hypervisor is behind a trait so a second backend does not change the CLI, viewer or MCP code.                                            |
| NFR-MNT-3 | The protocol carries a version number and the host and agent negotiate it.                                                                   |

## 10. User experience specification

### 10.1 Viewer layout

```
 ◆ terminal-windows   work   Windows · 1280×800 · 1280×800 px        ● live  24 fps  18 ms
╭──────────────────────────────────────────────────────────────────────────────────────╮
│                                                                                      │
│                              (guest desktop image)                                   │
│                                                                                      │
╰──────────────────────────────────────────────────────────────────────────────────────╯
 ^G Windows shortcuts   click   drag select   wheel scroll   type keyboard         ^] quit
```

- Header: brand badge, VM name, display size, link state, fps, latest action latency.
- Frame: rounded border that highlights during a drag or when the palette is open.
- Footer: context hints; the latest action toast replaces hints for 2.5 seconds (6 seconds for errors).
- Link messages, centered in the desktop area while no frame has arrived: connecting, at lock screen (with how to proceed), agent not responding, waiting for guest.

### 10.2 Windows shortcut palette (`Ctrl+G`)

| Key | Label         | Chord sent               |
| --- | ------------- | ------------------------ |
| `s` | Start menu    | `win`                    |
| `t` | Task view     | `win+tab`                |
| `a` | Switch app    | `alt+tab`                |
| `d` | Show desktop  | `win+d`                  |
| `r` | Run           | `win+r`                  |
| `e` | File Explorer | `win+e`                  |
| `w` | Close window  | `alt+f4`                 |
| `m` | Task Manager  | `ctrl+shift+esc`         |
| `i` | Settings      | `win+i`                  |
| `c` | Copy          | `ctrl+c`                 |
| `v` | Paste         | `ctrl+v`                 |
| `x` | Cut           | `ctrl+x`                 |
| `z` | Undo          | `ctrl+z`                 |
| `f` | Find          | `ctrl+f`                 |
| `l` | Ctrl+Alt+Del  | host-side `ctrl+alt+del` |

### 10.3 Key names accepted by `key`

Letters `a`-`z`, digits `0`-`9`, `return`/`enter`, `tab`, `space`, `backspace`, `delete`, `escape`/`esc`, `home`, `end`, `pageup`, `pagedown`, `insert`, `up`, `down`, `left`, `right`, `f1`-`f12`, punctuation (`-`, `=`, `[`, `]`, `\`, `;`, `'`, `,`, `.`, `/`, backtick). Modifiers: `ctrl`/`control`, `alt`/`option`, `shift`, `win`/`super`/`meta`. Example: `win+r`, `ctrl+shift+esc`. `cmd` is accepted as an alias of `win` for skill portability and documented as such.

### 10.4 Exit codes

| Code | Meaning                                                                     |
| ---- | --------------------------------------------------------------------------- |
| 0    | Success                                                                     |
| 1    | General error                                                               |
| 2    | Usage error (bad arguments)                                                 |
| 3    | Host prerequisite missing (Hyper-V not available, virtualization disabled)  |
| 4    | VM does not exist                                                           |
| 5    | VM is not running                                                           |
| 6    | Guest not ready or agent timeout                                            |
| 7    | Insufficient rights (elevation or Hyper-V Administrators membership needed) |
| 8    | Operation locked by another running operation                               |

### 10.5 Example transcripts

```text
> terminal-windows up --plan
{
  "name": "default",
  "steps": [
    "validate-windows-iso",
    "create-hyper-v-vm",
    "install-windows-unattended",
    "provision-guest-agent",
    "start-or-resume-vm",
    "wait-for-guest-agent",
    "open-terminal-view-if-supported"
  ]
}
```

```text
> terminal-windows action status
{
  "agent": "0.1.0",
  "dpi": "100",
  "height": "800",
  "session": "ready",
  "state": "running",
  "width": "1280"
}
```

## 11. Security and isolation model (product view)

- **Separate computer.** The guest has its own apps, files, accounts and settings. It does not inherit the host's signed-in sessions. It does consume host CPU, memory and disk.
- **No host input or screen APIs are used for agent actions.** All actions run inside the guest through the agent. The only host-side input path is the hypervisor keyboard device, used solely for the secure attention sequence and for typing at a lock or sign-in screen, and only on explicit user action in the viewer.
- **Control channel.** Hyper-V sockets are a host-to-VM stream not exposed on any network. The channel is additionally authenticated with a per-VM secret so other local users or processes cannot drive the guest.
- **No remote listener and no shell action.** The protocol has fixed actions with bounded arguments.
- **Networking.** Default NAT gives the guest internet access and may let it reach services on the host or local network. This is **not** a hardened environment for hostile code. `--offline` removes the network adapter.
- **Guest agent safety.** It only runs on a virtual machine. It never reads host files.
- **Data location.** VM data is stored per user under `%LOCALAPPDATA%\terminal-windows\`. Anyone who can read those files or administer Hyper-V on the host can read the guest.

## 12. Constraints, assumptions and dependencies

### 12.1 Constraints

- Hyper-V is available only on Windows Pro, Enterprise and Education.
- Hyper-V management needs elevation or membership in the _Hyper-V Administrators_ group.
- The user must hold a valid license (or use an evaluation) for the guest Windows.
- Windows 11 guests need a Generation 2 VM with Secure Boot and a virtual TPM.
- Hyper-V sockets use a service ID that is registered on the host in a one-time elevated step. Whether host-initiated connections strictly require the registration is confirmed by spike S5; the product registers it regardless.
- Windows Terminal does not implement the Kitty graphics protocol; the product relies on Sixel there.

### 12.2 Assumptions (to be validated by the Phase 0 spikes in TDD section 19)

- The Desktop Duplication API (or the GDI fallback) works in the Hyper-V basic-display guest session.
- Hyper-V Enhanced Session Mode can be prevented from taking the console session away from the agent.
- A secondary ISO built with the in-box IMAPI2 file-system API is found by Windows Setup as `autounattend.xml` media, first-logon commands run elevated for the administrator account, and the ISO and its edition list can be read without an elevated prompt.
- The synthetic video device supports 1280×800.
- A host-initiated Hyper-V socket connection to a guest listener works with the registration step above, and Tokio can drive the socket (spike S5).
- The hypervisor keyboard device (`Msvm_Keyboard`) accepts key presses and typed text, and the target terminals answer the cell-size, device-attribute and graphics queries the viewer relies on (spike S6).
- Hyper-V can store and run the VM from the user's data directory, and a Standard checkpoint of a VM with a virtual TPM restores cleanly with the control channel reconnecting (spike S7).

### 12.3 Dependencies

| Dependency                                            | Use                    |
| ----------------------------------------------------- | ---------------------- |
| Windows Hyper-V feature and PowerShell Hyper-V module | VM lifecycle           |
| Windows ISO supplied by the user                      | Guest OS               |
| Rust crates (summarized in TDD section 3)             | Implementation         |
| A graphics-capable terminal                           | Best viewer experience |
| GitHub or equivalent for releases                     | Distribution           |

## 13. Risks

| ID  | Risk                                                                                                                                       | Likelihood | Impact | Mitigation                                                                                                                                                                                  |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------ | ---------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R1  | Desktop Duplication does not work in the Hyper-V basic session                                                                             | Medium     | High   | Spike S1. GDI `BitBlt` fallback is designed in.                                                                                                                                             |
| R2  | Enhanced Session Mode moves the interactive session to an RDP session and blinds the agent                                                 | Medium     | High   | Spike S3. Disable the guest's RDP services during provisioning; our viewer does not use `vmconnect` by default.                                                                             |
| R3  | Windows Setup does not pick up the answer file from the answer ISO                                                                         | Low        | High   | Spike S4. Fallback: inject the answer file into the install ISO (Deployment Image Servicing tools).                                                                                         |
| R4  | UAC secure desktop and lock screen block capture and input                                                                                 | High       | Medium | Auto-logon, no idle lock, UAC prompt policy option, host-side keyboard fallback, v2 SYSTEM service plus session helper.                                                                     |
| R5  | Windows Terminal Sixel throughput is too low for smooth video                                                                              | Medium     | Medium | Cap Sixel fps, quantize and encode efficiently, recommend WezTerm for Kitty or iTerm2 images, half-block fallback.                                                                          |
| R6  | Cell pixel size is not reported by the terminal, breaking aspect or click mapping                                                          | Medium     | Medium | Try `CSI 16 t`, `CSI 14 t`, then environment override `TW_CELL_SIZE`, then a safe default with a visible hint.                                                                              |
| R7  | Hyper-V sockets behave differently with async runtimes                                                                                     | Low        | Medium | Spike S5. Fallback to blocking sockets on dedicated threads.                                                                                                                                |
| R8  | The PowerShell backend is slow or breaks across Windows builds                                                                             | Low        | Medium | Only lifecycle operations use it. Hot paths go over the Hyper-V socket. Trait allows a WMI backend.                                                                                         |
| R9  | Licensing confusion for Windows guests                                                                                                     | Medium     | Medium | Clear docs; the tool never distributes Windows; evaluation ISOs are supported. Evaluation media stops being usable after its evaluation period (for example 90 days), which the docs state. |
| R10 | Users on Windows Home cannot use v1                                                                                                        | High       | Medium | Documented; QEMU + WHPX backend planned (P1).                                                                                                                                               |
| R11 | Guest display size options are limited by the synthetic video device                                                                       | Medium     | Low    | Spike S2. Fall back to a supported resolution list.                                                                                                                                         |
| R12 | Antivirus flags a guest agent that injects input and captures the screen                                                                   | Medium     | Medium | Authenticode signing, a stable publisher identity, documentation.                                                                                                                           |
| R13 | Hyper-V cannot use the user's data directory as VM storage, or a checkpoint of a vTPM VM does not restore cleanly with the control channel | Low        | Medium | Spike S7. Fall back to a Hyper-V default storage path, or Production checkpoints.                                                                                                           |

## 14. Success metrics

| Metric                                                                                     | Target                                                          |
| ------------------------------------------------------------------------------------------ | --------------------------------------------------------------- |
| Time from install to first visible desktop (excluding ISO download and Windows setup time) | Under 5 minutes of user attention, with zero guest interactions |
| Manual guest steps during setup                                                            | 0                                                               |
| MCP tool compatibility with `terminal-mac` skills (tool names and argument shapes)         | 100 percent of the eight tools                                  |
| Input round-trip median                                                                    | Under 30 ms                                                     |
| Viewer exits that leave the terminal broken                                                | 0 in acceptance testing                                         |
| Acceptance checklist pass rate before 1.0                                                  | 100 percent of P0 items                                         |

## 15. Release plan

| Milestone                            | Content                                                                                                                                                                    | Exit criteria                                                                 |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| **M0: Spikes**                       | Validate the assumptions in 12.2 on a real Hyper-V host.                                                                                                                   | Spikes S1 to S7 in the TDD have written results; design adjustments recorded. |
| **M1: Foundations**                  | Workspace, protocol crate, models, argument parsing, Hypervisor trait, Hyper-V backend lifecycle (create, start, stop, pause, resume, status, ls), `doctor`, `setup-host`. | Lifecycle works against a manually installed Windows VM.                      |
| **M2: Unattended install and agent** | ISO validation, answer media, unattended install, agent install, handshake, `status`, `screenshot`, input actions.                                                         | `up` produces a controllable desktop with no guest interaction.               |
| **M3: Agent interfaces**             | `action` CLI, MCP server, skill, `mcp --config`.                                                                                                                           | An MCP client completes a multi-step GUI task.                                |
| **M4: Viewer**                       | Terminal I/O layer, Kitty, iTerm2, Sixel renderers, layout, palette, link states.                                                                                          | Viewer acceptance checklist passes in Windows Terminal and WezTerm.           |
| **M5: Polish and packaging**         | Checkpoints, push updates, logs, `rm`, installer, signing, CI, docs.                                                                                                       | Release candidate installs from a clean machine.                              |
| **M6: Home support**                 | QEMU + WHPX backend, Windows Home path.                                                                                                                                    | Same acceptance checklist on a Windows Home machine.                          |
| **Later**                            | Linux guests, SYSTEM service plus session helper (secure desktop), multi-monitor, Windows on Arm, hardening of guest networking, `winget`.                                 | Per feature.                                                                  |

## 16. Open questions and decisions

| ID  | Question                                                                             | Provisional decision                                                                                                                                    | Owner         |
| --- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------- |
| D1  | Hypervisor for v1: Hyper-V, or QEMU + WHPX (also works on Home)?                     | **Hyper-V first**, behind a trait; QEMU + WHPX in M6. Hyper-V gives vTPM (Windows 11), Hyper-V sockets, checkpoints and a supported management surface. | Product owner |
| D2  | Guest OS for v1                                                                      | **Windows 11 (and 10)**. Linux later.                                                                                                                   | Product owner |
| D3  | Default guest display size                                                           | 1280×800, validated by spike S2.                                                                                                                        | Engineering   |
| D4  | Binary names                                                                         | `terminal-windows` (CLI) and `twagent` (guest agent).                                                                                                   | Engineering   |
| D5  | Agent update model                                                                   | Push over the channel after the first install.                                                                                                          | Engineering   |
| D6  | Whether to ship the v2 "service plus session helper" (secure desktop support) in 1.0 | **No.** Ship the interactive-session agent in 1.0; document UAC and lock limits and the mitigations.                                                    | Product owner |
| D7  | Default UAC behaviour in the guest                                                   | Keep the Windows default, offer `--uac off` (no secure desktop prompts) for agent workloads.                                                            | Product owner |
| D8  | Distribution channel                                                                 | GitHub Releases ZIP and `get.ps1`. `winget` later.                                                                                                      | Product owner |

## 17. Glossary

| Term                                 | Meaning                                                                                                      |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------ |
| Agent (guest agent)                  | `twagent.exe`, the program inside the guest that captures the screen and injects input.                      |
| Host                                 | The user's physical Windows PC.                                                                              |
| Guest                                | The Windows operating system inside the VM.                                                                  |
| Hyper-V socket (`AF_HYPERV`, hvsock) | A socket type for host-to-VM communication that does not use the network stack.                              |
| Service ID                           | A GUID that names a Hyper-V socket service, registered on the host.                                          |
| WHPX                                 | Windows Hypervisor Platform, the user-mode virtualization API that QEMU and others use on Windows.           |
| UIPI                                 | User Interface Privilege Isolation: lower-integrity processes cannot send input to higher-integrity windows. |
| Secure desktop                       | The isolated desktop used for UAC prompts and the sign-in and lock screens.                                  |
| Desktop Duplication                  | A DXGI API that provides desktop frames and dirty rectangles.                                                |
| Sixel                                | A raster graphics protocol for terminals.                                                                    |
| Standard checkpoint                  | A Hyper-V checkpoint that also saves memory and device state.                                                |
| MCP                                  | Model Context Protocol, the tool protocol used by agent clients.                                             |
| Mailbox                              | `terminal-mac`'s file-based request and reply channel. Replaced by the Hyper-V socket here.                  |

---

## Appendix A: capability parity matrix

Legend: **Same** = equivalent behaviour. **Adapted** = same intent, different mechanism. **Improved** = better on Windows. **Replaced** = macOS-specific, replaced with a Windows mechanism. **New** = not in `terminal-mac`.

| #   | `terminal-mac` capability                                           | `terminal-windows` equivalent                                                                    | Status   | Pri   |
| --- | ------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ | -------- | ----- |
| 1   | Isolated VM running a full desktop OS                               | Hyper-V Generation 2 VM running Windows                                                          | Adapted  | P0    |
| 2   | Guest OS install from Apple restore image, downloaded automatically | Guest OS install from a user-supplied Windows ISO, validated, unattended                         | Replaced | P0    |
| 3   | Install is guided, with manual Setup Assistant                      | Fully unattended install and account creation                                                    | Improved | P0    |
| 4   | Guest helper app captures screen (ScreenCaptureKit)                 | Guest agent captures screen (Desktop Duplication, GDI fallback)                                  | Adapted  | P0    |
| 5   | Guest helper posts input (`CGEvent`)                                | Guest agent posts input (`SendInput`)                                                            | Adapted  | P0    |
| 6   | Guest permissions (Screen Recording, Accessibility)                 | Not required; agent runs in the interactive session at high integrity                            | Replaced | P0    |
| 7   | Helper installs itself, registers as login item                     | Setup media installs the agent; scheduled task at logon; auto logon                              | Replaced | P0    |
| 8   | Helper self-update from shared tools folder                         | Push update over the control channel with hash verification                                      | Adapted  | P1    |
| 9   | Guest refuses to run on a physical Mac (`hw.model`)                 | Guest refuses to run outside a Hyper-V guest                                                     | Adapted  | P0    |
| 10  | virtio-fs file mailbox transport                                    | Hyper-V socket stream with framing and authentication                                            | Replaced | P0    |
| 11  | Symlink, hardlink and size defenses                                 | Message size bounds, strict decoding, authenticated peer                                         | Adapted  | P0    |
| 12  | Request IDs, deadlines, reply matching                              | Same request and reply model with IDs and deadlines                                              | Same     | P0    |
| 13  | Frames only on change, up to 30 fps                                 | Same, pushed by the agent with backpressure                                                      | Improved | P0    |
| 14  | Frames pre-scaled to viewer size, lz4 compressed                    | Same                                                                                             | Same     | P0    |
| 15  | Capture stops 15 s after viewer leaves                              | Capture stops when the subscription ends (short grace)                                           | Improved | P0    |
| 16  | Kitty shared memory transport                                       | Not applicable on Windows. Kitty PNG, iTerm2, Sixel, half-blocks instead                         | Replaced | P0/P1 |
| 17  | Terminal viewer with header, frame, footer, link states             | Same layout and states                                                                           | Same     | P0    |
| 18  | Mouse: click, right click, double and triple, drag, scroll          | Same                                                                                             | Same     | P0    |
| 19  | Typing as Unicode events, no clipboard use                          | Same, via `KEYEVENTF_UNICODE`                                                                    | Same     | P0    |
| 20  | Key chords with modifiers (`cmd`, `ctrl`, `alt`, `shift`)           | Chords with `ctrl`, `alt`, `shift`, `win` (`cmd` accepted as alias of `win`)                     | Adapted  | P0    |
| 21  | `Ctrl+G` shortcut palette (Mac shortcuts)                           | Palette of Windows shortcuts                                                                     | Adapted  | P0    |
| 22  | `Ctrl+]` exit, VM keeps running                                     | Same                                                                                             | Same     | P0    |
| 23  | `Ctrl+O` opens the native VM window to sign in                      | Opens `vmconnect`; host-side typing for lock screen                                              | Adapted  | P1    |
| 24  | Native VM window `show` and `hide`                                  | `show` and `hide` through `vmconnect`                                                            | Adapted  | P1    |
| 25  | `--read-only` viewer                                                | Same                                                                                             | Same     | P0    |
| 26  | `--protocol kitty\|iterm`                                           | `--protocol kitty\|iterm\|sixel\|blocks`                                                         | Adapted  | P0    |
| 27  | HiDPI (2x) guest display                                            | Fixed pixel display; guest DPI scale reported; agent is per-monitor DPI aware                    | Adapted  | P1    |
| 28  | `display --hidpi\|--standard`                                       | `display --size WxH`                                                                             | Adapted  | P1    |
| 29  | VM lifecycle: create, start, stop, pause, resume, status, ls        | Same, through Hyper-V                                                                            | Same     | P0    |
| 30  | `stop --force`                                                      | Same (`Stop-VM -TurnOff`)                                                                        | Same     | P0    |
| 31  | Exclusive VM lock, detached `__run` process owns the VM             | Hyper-V owns the VM; the tool uses an operation lock only                                        | Improved | P0    |
| 32  | Sparse disk                                                         | Dynamically expanding VHDX                                                                       | Same     | P0    |
| 33  | Offline option (no NIC)                                             | Same                                                                                             | Same     | P0    |
| 34  | NAT networking default                                              | Hyper-V Default Switch (NAT)                                                                     | Same     | P0    |
| 35  | `up` guided flow with plan, no-view, detach, timeout                | Same flags                                                                                       | Same     | P0    |
| 36  | Restore-image download with resume                                  | Not applicable (ISO is user supplied)                                                            | Replaced | n/a   |
| 37  | MCP server with eight tools                                         | Same eight tools, same shapes                                                                    | Same     | P0    |
| 38  | `mcp --config`                                                      | Same                                                                                             | Same     | P0    |
| 39  | Bundled skill and `skills get/path/install`                         | Same                                                                                             | Same     | P0    |
| 40  | Claude plugin and marketplace manifest                              | Same                                                                                             | Same     | P1    |
| 41  | `doctor`                                                            | Extended (Hyper-V, rights, registration)                                                         | Adapted  | P0    |
| 42  | JSON output and nonzero exit on failure                             | Same, with a documented exit-code table                                                          | Same     | P0    |
| 43  | `TERMINAL_MAC_HOME`                                                 | `TERMINAL_WINDOWS_HOME`                                                                          | Same     | P0    |
| 44  | Self-signed app, curl installer, checksum                           | ZIP, `install.ps1`, `get.ps1`, checksum, Authenticode                                            | Adapted  | P0/P1 |
| 45  | No VM delete command (by design)                                    | `rm --yes`                                                                                       | Improved | P1    |
| 46  | Checkpoints and rollback (listed as future work)                    | Standard checkpoints and restore                                                                 | New      | P1    |
| 47  | Host-side typing at login screen                                    | Hypervisor keyboard device                                                                       | New      | P1    |
| 48  | Ctrl+Alt+Del                                                        | Hypervisor keyboard device                                                                       | New      | P1    |
| 49  | Accessibility-tree snapshot (not implemented)                       | Not implemented (alias of screenshot)                                                            | Same     | n/a   |
| 50  | Tests: mailbox, validation, input parsing                           | Tests: protocol, validation, input parsing, layout, key map, coordinate mapping, fake hypervisor | Same     | P0    |

## Appendix B: reference facts taken from `terminal-mac`

These facts, read from the source, anchor the parity targets.

| Fact                                                                                           | Source file                           |
| ---------------------------------------------------------------------------------------------- | ------------------------------------- |
| Default 1280×800 points, 4 CPUs max default, memory at least 4 GiB, 64 GiB disk, HiDPI scale 2 | `VirtualMachine.swift`                |
| Mailbox: request file limit 262144 bytes, up to 32 requests per poll, 1 MiB default read limit | `Mailbox.swift`                       |
| Frame limit 64 MiB, screenshot limit 32 MiB in the CLI path                                    | `Viewer.swift`, `CLI.swift`           |
| Capture capped at 30 fps; stream stops after 15 s without use                                  | `GuestCapture.swift`                  |
| `type` text limit 32 KiB; scroll range ±1000; click count 1 to 3                               | `Guest.swift`                         |
| MCP tools: `screenshot`, `click`, `move`, `drag`, `scroll`, `type`, `key`, `status`            | `AgentServer.swift`                   |
| Viewer: SGR mouse, `Ctrl+G` palette, `Ctrl+]` exit, `Ctrl+O` window                            | `Viewer.swift`, `TerminalInput.swift` |
| Guest readiness = both permissions true                                                        | `Setup.swift`                         |
| VM names `^[a-zA-Z0-9][a-zA-Z0-9_-]{0,47}$`                                                    | `Models.swift`                        |
