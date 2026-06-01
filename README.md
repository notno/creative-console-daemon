# Creative Console Daemon

A Rust background daemon that reads button presses from the **Logitech MX Creative Console** or **Elgato Stream Deck XL** via USB HID and dispatches configurable actions: OBS control, HTTP webhooks, media keys, and webhook polling for reactive button states.

Built as a replacement for Logitech Options+, which is broken and limited.

## Features

- **Dual device support** — MX Creative Console (Keypad) and Stream Deck XL
- **Direct HID communication** — no dependency on Logitech Options+ or Elgato Stream Deck software
- **OBS control** via obs-websocket v5 (switch scenes, start/stop recording, toggle mute)
- **HTTP webhooks** — trigger POST/GET/DELETE requests on button press
- **Webhook polling** — periodically poll a JSON endpoint to drive button active states (e.g. spotlight indicators)
- **Media keys** — play/pause, volume, next/prev track via Windows SendInput
- **LCD button labels** — render text labels on both MX Creative and Stream Deck button screens
- **TOML configuration** — define all button mappings in a simple config file
- **Config hot-reload** — edit config.toml while running, changes apply automatically
- **Supervisor script** — auto-restart on device disconnect

## Requirements

- Windows 11
- Rust toolchain (for building)
- One or both of:
  - Logitech MX Creative Console (Keypad, USB-C connected)
  - Elgato Stream Deck XL (USB connected)
- OBS with obs-websocket plugin (for OBS actions)

## Building

```bash
cargo build --release
```

The binary will be at `target/release/creative-console-daemon.exe`.

## Configuration

Copy `config.example.toml` (MX Creative) or `config.example.streamdeck.toml` (Stream Deck XL) to `config.toml` and edit.

### Device Selection

Select your device with the `device_type` field:

```toml
[device]
device_type = "mx_creative"    # Logitech MX Creative Console (default)
# device_type = "streamdeck_xl"  # Elgato Stream Deck XL
```

For Stream Deck, you can optionally specify a serial number to target a specific device:

```toml
[device]
device_type = "streamdeck_xl"
serial = "AL12H1A00001"
```

### Using Both Devices

Run two instances of the daemon with separate config files — one per device:

```bash
# Terminal 1: MX Creative Console
creative-console-daemon --config config.mx.toml

# Terminal 2: Stream Deck XL
creative-console-daemon --config config.streamdeck.toml
```

Each instance independently connects to its configured device. They can share the same OBS WebSocket, webhook endpoints, and ttrpg-ai server without conflict. This lets you use the MX Creative for OBS/media controls while the Stream Deck XL handles spotlight and session buttons (or any other split you prefer).

### MX Creative Console Example

```toml
[device]
device_type = "mx_creative"

[obs]
host = "localhost"
port = 4455

# Button IDs: 1-9 = LCD buttons (3x3 grid), 10 = PageLeft, 11 = PageRight
[[button]]
id = 1
action = "obs"
command = "SetCurrentProgramScene"
params = { sceneName = "Camera 1" }

[[button]]
id = 2
action = "obs"
command = "ToggleRecord"

[[button]]
id = 3
action = "media"
key = "play_pause"
```

### Stream Deck XL Example

```toml
[device]
device_type = "streamdeck_xl"

# Button IDs: 1-32 (8x4 grid, left-to-right, top-to-bottom)
# Row 1: 1-8, Row 2: 9-16, Row 3: 17-24, Row 4: 25-32

[[button]]
id = 1
label = "Slot 1"
[button.action]
type = "webhook"
method = "POST"
url = "http://localhost:3000/api/spotlight/1"

[[button]]
id = 25
label = "PTT ON"
[button.action]
type = "webhook"
method = "POST"
url = "http://localhost:3000/api/ptt/on"
```

### Webhook Polling

Poll a JSON endpoint periodically to update button active states (highlighted/dimmed). Useful for showing live state like which spotlight slot is active:

```toml
[[webhook_poll]]
url = "http://localhost:3000/api/spotlight"
interval_secs = 2

[webhook_poll.buttons]
# button_id = "json.path.to.boolean"
1 = "slots.1.spotlit"
2 = "slots.2.spotlit"
3 = "slots.3.spotlit"
```

The poller fetches the URL, walks each dot-separated JSON path, and treats the result as a boolean. Active buttons are rendered with a highlight color; inactive buttons are dimmed.

### Supported Actions

| Action | Fields | Description |
|--------|--------|-------------|
| `obs` | `command`, `params` | Send command to OBS via WebSocket |
| `webhook` | `method`, `url`, `body`, `headers`, `release_url` | Send HTTP request (optional second request on release) |
| `media` | `key` | Simulate media key press |
| `hotkey` | `keys`, `hold` | Send a key combo via SendInput |
| `shell` | `cmd`, `args`, `output`, `trim` | Run a command, optionally capture stdout |

**OBS commands:** `SetCurrentProgramScene`, `StartRecord`, `StopRecord`, `ToggleRecord`, `ToggleInputMute`

**Media keys:** `play_pause`, `volume_up`, `volume_down`, `mute`, `next_track`, `prev_track`

**Hotkey:** `keys` is a list pressed together and released in reverse (e.g. `["ctrl", "win"]`). Set `hold = true` for push-to-talk (keys stay down while the button is held); the default taps and releases immediately.

**Shell:** `output` is `none`, `clipboard`, or `paste` (default `paste` — copies stdout then sends Ctrl+V). `trim = true` (default) strips surrounding whitespace from stdout first.

```toml
[[button]]
id = 4
label = "Ctrl+Win"
[button.action]
type = "hotkey"
keys = ["ctrl", "win"]

[[button]]
id = 5
label = "Paste Shot"
[button.action]
type = "shell"
cmd = "powershell"
args = ["-NoProfile", "-Command", "Get-Date -Format o"]
output = "paste"
```

### Button ID Reference

| Device | Button IDs | Layout |
|--------|-----------|--------|
| MX Creative | 1-9 (LCD), 10-11 (page) | 3x3 grid + 2 page buttons |
| Stream Deck XL | 1-32 | 8x4 grid |

## Usage

```bash
# Run the daemon
creative-console-daemon --config config.toml

# List all connected devices (MX Creative + Stream Deck)
creative-console-daemon --list-devices

# Raw dump mode (print HID reports as hex, MX Creative only)
creative-console-daemon --raw-dump --config config.toml

# Diagnostic mode (probe HID interfaces)
creative-console-daemon --diag

# Dry run (log button presses, don't dispatch actions)
creative-console-daemon --dry-run --config config.toml
```

### Auto-restart on Disconnect

The daemon exits with code 2 when the device disconnects. Use the supervisor script for automatic restart:

```powershell
.\restart.ps1
```

Or via batch file:

```cmd
restart.bat
```

### Run at System Startup

Register a Windows Scheduled Task that runs a supervisor hidden at user logon. This is the recommended option — OBS WebSocket and Windows media keys both require a user session, so a true boot-time service offers no benefit.

By default the installer wires up `editor.exe`: a windowless tray app that supervises the daemon(s) (auto-restart on device disconnect) and hosts the button/page editor. Right-click the tray icon for **Open editor / Open logs / Start daemons / Stop daemons / Restart daemons / Quit**; double-click to open the editor. Pass `-Headless` to use `restart.ps1` instead (no tray/editor, console supervisor, single config).

**One daemon runs per config.** Pass an MX config and a Stream Deck config and both devices run at once; a device that isn't plugged in just leaves its daemon stopped. The default config set is `config.ctrl-win.toml` + `config.streamdeck.toml`.

```powershell
# Build both binaries first
cargo build --release --workspace

# Install with tray app + editor (default: MX + Stream Deck configs)
.\install-task.ps1

# Just one device
.\install-task.ps1 -Configs config.ctrl-win.toml

# Install headless (no tray, just supervisor — single config)
.\install-task.ps1 -Headless -Configs config.ctrl-win.toml

# Test immediately without logging out
Start-ScheduledTask -TaskName CreativeConsoleDaemon

# Check status
Get-ScheduledTask -TaskName CreativeConsoleDaemon | Get-ScheduledTaskInfo

# Remove
.\uninstall-task.ps1
```

The task runs as the current user with `Limited` (non-elevated) privileges, no time limit, and will restart up to 3 times on failure. Both modes handle exit-code-2 restarts on device disconnect. A Win32 Job Object ties the daemon children to the tray process, so they're terminated even if the tray process is force-killed.

If you fully **Quit** the tray app, relaunch it with `.\launch.ps1` (or re-run the task). The **Start daemons** tray item only restarts the daemons, not the whole app.

### Editing buttons

Open the editor from the tray icon (**Open editor**, or double-click the icon). It's a GUI for button actions, labels, colors, and pages. When more than one config is open, a dropdown at the top switches which one you're editing (each keeps its own unsaved edits). Saving writes that config; its daemon hot-reloads and re-renders the device — no restart needed. Saving regenerates the file in a canonical form, so hand-written comments are not preserved.

**Copy / paste buttons.** Right-click a button for **Copy / Cut / Paste / Delete**, or use **Ctrl+C / Ctrl+X / Ctrl+V** and **Delete** on the selected cell (these defer to the focused text field while you're typing in one). The clipboard is shared across tabs, so you can copy a button from one config or device and paste it into another — the pasted button takes the target cell's id/page and replaces whatever is there. **Ctrl+S** saves the active tab.

**Undo / redo.** Structural edits (create, delete, cut, paste) are undoable per config — **Ctrl+Z** to undo, **Ctrl+Y** or **Ctrl+Shift+Z** to redo, or use the Undo/Redo buttons in the top bar (history depth 50). Field edits (label text, colors, params) aren't on this stack, but text fields self-undo with Ctrl+Z while focused.

`editor.exe` has two modes: launched with config paths (or no args) it runs as the **resident tray + supervisor** (no window), one daemon per config; launched with `--edit` it opens the **editor window** with a tab per config. The tray spawns the editor window for you, so you normally never pass `--edit` yourself.

```powershell
target\release\editor.exe --edit config.ctrl-win.toml config.streamdeck.toml
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean shutdown (Ctrl+C) |
| 1 | Fatal error (bad config, device not found) |
| 2 | Device disconnected (supervisor should restart) |

## Logging

Set the `RUST_LOG` environment variable to control log verbosity:

```bash
RUST_LOG=debug creative-console-daemon --config config.toml
```

## Important Notes

- **Stop Logitech Options+** before running the MX Creative daemon. While HID access is non-exclusive, Options+ may consume button events.
- **Stop Elgato Stream Deck software** before running the Stream Deck daemon, for the same reason.
- The MX Creative Keypad must be connected via **USB-C**. The Dialpad (Bluetooth) is not supported.
- Stream Deck XL is auto-detected; if multiple Stream Decks are connected, use the `serial` field to target a specific one.

## Known Limitations

- MX Creative: Keypad only (no Dialpad/dial support)
- Stream Deck: XL tested; other models may work but are untested
- No per-application profiles
- Windows only
