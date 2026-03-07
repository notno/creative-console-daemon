# Creative Console Daemon

A Rust background daemon that reads button presses from the **Logitech MX Creative Console** (Keypad) via USB HID and dispatches configurable actions: OBS control, HTTP webhooks, and media keys.

Built as a replacement for Logitech Options+, which is broken and limited.

## Features

- **Direct HID communication** — no dependency on Logitech Options+
- **OBS control** via obs-websocket v5 (switch scenes, start/stop recording, toggle mute)
- **HTTP webhooks** — trigger POST/GET requests on button press
- **Media keys** — play/pause, volume, next/prev track via Windows SendInput
- **TOML configuration** — define all button mappings in a simple config file
- **Supervisor script** — auto-restart on device disconnect

## Requirements

- Windows 11
- Rust toolchain (for building)
- Logitech MX Creative Console (Keypad, USB-C connected)
- OBS with obs-websocket plugin (for OBS actions)

## Building

```bash
cargo build --release
```

The binary will be at `target/release/creative-console-daemon.exe`.

## Configuration

Copy `config.example.toml` to `config.toml` and edit:

```toml
# Device settings (optional — defaults to MX Creative Keypad)
[device]
# vendor_id = 0x046D
# product_id = 0xC354
# usage_page = 0xFF00

# OBS WebSocket connection (optional)
[obs]
host = "localhost"
port = 4455
# password = "your-password"

# Button mappings
# IDs: 1-9 = LCD buttons (3x3 grid), 10 = PageLeft, 11 = PageRight

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

[[button]]
id = 4
action = "webhook"
method = "POST"
url = "http://localhost:8080/api/trigger"
```

### Supported Actions

| Action | Fields | Description |
|--------|--------|-------------|
| `obs` | `command`, `params` | Send command to OBS via WebSocket |
| `webhook` | `method`, `url`, `body`, `headers` | Send HTTP request |
| `media` | `key` | Simulate media key press |

**OBS commands:** `SetCurrentProgramScene`, `StartRecord`, `StopRecord`, `ToggleRecord`, `ToggleInputMute`

**Media keys:** `play_pause`, `volume_up`, `volume_down`, `mute`, `next_track`, `prev_track`

## Usage

```bash
# Run the daemon
creative-console-daemon --config config.toml

# List connected HID devices (for debugging)
creative-console-daemon --list-devices

# Raw dump mode (print HID reports as hex)
creative-console-daemon --raw-dump --config config.toml

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

- **Stop Logitech Options+** before running this daemon. While HID access is non-exclusive, Options+ may consume button events.
- The device must be connected via **USB-C** (the Keypad). The Dialpad (Bluetooth) is not supported in this version.
- Button IDs 1-9 correspond to the 3x3 LCD button grid (left-to-right, top-to-bottom). IDs 10-11 are the page/arrow buttons.

## Known Limitations

- Keypad only (no Dialpad/dial support)
- No LCD screen image output
- No per-application profiles
- No config hot-reload (restart daemon to apply changes)
- Windows only
