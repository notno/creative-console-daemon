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
| `webhook` | `method`, `url`, `body`, `headers` | Send HTTP request |
| `media` | `key` | Simulate media key press |

**OBS commands:** `SetCurrentProgramScene`, `StartRecord`, `StopRecord`, `ToggleRecord`, `ToggleInputMute`

**Media keys:** `play_pause`, `volume_up`, `volume_down`, `mute`, `next_track`, `prev_track`

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
