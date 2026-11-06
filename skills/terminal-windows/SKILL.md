---
name: terminal-windows
description: Operate a separate virtual Windows PC (Hyper-V or VirtualBox) with screenshots, clicks, typing and shortcuts, without touching the user's own desktop. Use when a task needs a real Windows GUI: testing a Windows app, using a website or desktop app, or any computer-use task the user wants sandboxed.
---

# terminal-windows

`terminal-windows` runs a real Windows guest in a virtual machine (Hyper-V, or VirtualBox on Windows Home). A helper inside the guest captures its screen and posts input, so everything you do happens in the VM. It never controls the user's own desktop.

## Prefer the MCP tools

If tools named `screenshot`, `click`, `type`, `key` and so on from the `terminal-windows` MCP server are available, use them:

1. `status`: the VM is running and the session is `ready`.
2. `screenshot`: every x/y you pass to other tools is a pixel in this image (top-left origin).
3. Act with `click` (`clicks: 2` double-clicks, `button: "right"`), `move`, `drag`, `scroll` (negative `dy` scrolls down), `type` (literal text) or `key` (`return`, `tab`, `escape`, `win+r`, `ctrl+c`, `ctrl+shift+esc`, `alt+f4`...).
4. Look again: pass `screenshot: true` to an action, or call `screenshot`, before deciding the next step.

Opening an app: `key win`, `type "notepad"`, `key return`, or `key win+r`, `type "notepad"`, `key return`. Prefer keyboard shortcuts over hunting for small targets.

## Without MCP: the CLI

Commands exit nonzero on failure with a sentence saying what to do. `NAME` defaults to `default`.

```powershell
terminal-windows status NAME --json                    # stopped -> terminal-windows start NAME
terminal-windows screenshot NAME --out guest.png       # then view the PNG
terminal-windows action click x=640 y=400 --vm NAME
terminal-windows action type text="hello" --vm NAME
terminal-windows action key key=win+r --vm NAME
terminal-windows action scroll x=640 y=400 dy=-300 --vm NAME
terminal-windows action drag x=100 y=100 x2=300 y2=300 --vm NAME
```

Text is sent as Unicode keyboard events, not through the clipboard. No shell-execution action exists.

## When to stop and ask the user

- `The guest is locked`, `UAC prompt` or `secure desktop`: the agent cannot control the sign-in screen or UAC prompts. Ask the user to open the viewer (`terminal-windows view`) and handle it; `Ctrl+Alt+Delete` is available from the viewer menu (Ctrl+G, d).
- No VM yet: ask the user to run `terminal-windows up --iso PATH` once; it installs Windows unattended and needs a Windows 10/11 ISO and either Hyper-V (Pro, Enterprise, Education) or VirtualBox 7 (any edition, including Home).
- VM stopped or paused: ask the user to run `terminal-windows start` or `resume`.
- Anything destructive on the user's behalf (deleting the VM, signing in to their accounts, payments): ask first.

## Good habits

- One action, then look. Do not chain several blind clicks.
- Coordinates come from the latest screenshot only; the screen may have changed since.
- Use `checkpoint create LABEL` before risky steps and `checkpoint restore LABEL` to roll back.
