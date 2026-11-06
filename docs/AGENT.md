# Using terminal-windows from an AI agent

`terminal-windows` gives an agent computer use in a separate virtual Windows PC. Every action happens inside the VM through a guest agent; the user's own desktop, pointer and clipboard are never touched.

## Connect a client

Claude Code, as a plugin (MCP server and skill together):

```text
/plugin install terminal-windows --marketplace Aneesh-Madupalli/terminal-windows
```

Or add the pieces separately:

```powershell
claude mcp add terminal-windows -- terminal-windows mcp
terminal-windows skills install            # copies SKILL.md to ~\.claude\skills\terminal-windows
```

Other MCP clients (Cursor, VS Code, Codex, Windsurf, ...): paste the output of `terminal-windows mcp --config` into the client's MCP settings. Use `terminal-windows mcp NAME` for a machine other than `default`.

Any agent with a skills folder: `terminal-windows skills install --dir PATH`. In an `AGENTS.md`, a line such as "Before using the virtual Windows PC, run `terminal-windows skills get`" keeps the instructions in step with the installed version.

## Tools

| Tool                     | Purpose                                                                                                  |
| ------------------------ | -------------------------------------------------------------------------------------------------------- |
| `status`                 | Is the VM running, and is the guest session `ready`, `locked`, `no-session` or `secure-desktop`          |
| `screenshot`             | PNG of the guest. Every x and y you pass to other tools is a pixel in this image, origin at the top left |
| `click`                  | Left or right click; `clicks: 2` double-clicks                                                           |
| `move`, `drag`, `scroll` | Pointer movement, drag from one point to another, wheel (negative `dy` scrolls down)                     |
| `type`                   | Literal text, sent as Unicode keyboard events (not through the clipboard)                                |
| `key`                    | A key or chord: `return`, `tab`, `escape`, `win`, `win+r`, `ctrl+c`, `ctrl+shift+esc`, `alt+f4`          |

There is no tool that runs a shell command. Anything that can be done in the guest is done by looking at the screen and using input, the same as a person would.

## Working loop

1. `status`. If the VM is not ready, tell the user what the message says; do not retry in a loop.
2. `screenshot`.
3. Do one action.
4. Look again before the next action. Pass `screenshot: true` to an action or call `screenshot`.

Prefer keyboard shortcuts to small click targets. To open an app: `key win`, `type "notepad"`, `key return`.

## When to hand back to the user

| What you see         | Why                                                                      | What to ask                                   |
| -------------------- | ------------------------------------------------------------------------ | --------------------------------------------- |
| `locked`             | The guest is at the lock screen                                          | Sign in through `terminal-windows view`       |
| `secure-desktop`     | A UAC prompt or the sign-in screen is showing; the agent cannot reach it | Approve the prompt in `terminal-windows view` |
| `no-session`         | Nobody is signed in to the guest                                         | Sign in through `terminal-windows view`       |
| VM stopped or paused |                                                                          | `terminal-windows start` or `resume`          |
| No VM yet            |                                                                          | `terminal-windows up --iso PATH` (once)       |

`Ctrl+Alt+Delete` cannot be sent from inside the guest. The user can send it from the viewer: `Ctrl+G`, then `d`.

Ask before anything destructive or personal: deleting the VM, signing in to the user's accounts, payments.

## Safety notes

- Rolling back: `terminal-windows checkpoint create LABEL` before a risky step, `checkpoint restore LABEL` to undo it.
- The channel between host and guest is authenticated with a per-VM secret and, on Hyper-V, never leaves the host. On VirtualBox it is a TCP port bound to 127.0.0.1.
- The guest has its own accounts and files. It does not inherit the host's signed-in sessions. It does use host CPU, memory and disk.

## Without MCP

```powershell
terminal-windows status NAME --json
terminal-windows screenshot NAME --out guest.png
terminal-windows action click x=640 y=400 --vm NAME
terminal-windows action type text="hello" --vm NAME
terminal-windows action key key=win+r --vm NAME
terminal-windows action scroll x=640 y=400 dy=-300 --vm NAME
```

Commands exit nonzero on failure with a sentence saying what to do. Exit codes: 2 usage, 3 prerequisite missing, 4 no such VM, 5 VM not running, 6 guest not ready, 7 access denied, 8 guest locked.
