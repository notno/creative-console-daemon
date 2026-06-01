use anyhow::{Context, Result};
use std::time::Duration;
use tokio::process::Command;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_SHIFT,
};

const CF_UNICODETEXT: u32 = 13;

/// Output disposition for a shell action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOutput {
    /// Discard stdout.
    None,
    /// Copy trimmed stdout to clipboard.
    Clipboard,
    /// Copy trimmed stdout to clipboard, then send Ctrl+V to the focused window.
    Paste,
}

impl ShellOutput {
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "none" => Ok(Self::None),
            "clipboard" => Ok(Self::Clipboard),
            "paste" => Ok(Self::Paste),
            other => anyhow::bail!("Unknown shell output mode '{other}'. Valid: none, clipboard, paste"),
        }
    }
}

/// Spawn the configured shell command, capture stdout, and route to clipboard/paste as configured.
pub async fn run(cmd: &str, args: &[String], output: ShellOutput, trim: bool) -> Result<()> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .await
        .with_context(|| format!("Failed to spawn shell command: {cmd}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "Shell command exited with status {}: {}",
            out.status,
            stderr.trim()
        );
    }

    if matches!(output, ShellOutput::None) {
        tracing::info!(cmd, "Shell command finished (output discarded)");
        return Ok(());
    }

    let mut stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if trim {
        let trimmed = stdout.trim();
        stdout = trimmed.to_string();
    }
    if stdout.is_empty() {
        anyhow::bail!("Shell command produced no stdout for {output:?}");
    }

    set_clipboard_text(&stdout)?;
    tracing::info!(cmd, len = stdout.len(), "Shell stdout copied to clipboard");

    if matches!(output, ShellOutput::Paste) {
        // Brief delay so the clipboard owner change settles before the focused app reads it.
        tokio::time::sleep(Duration::from_millis(40)).await;
        send_paste_plain()?;
        tracing::info!("Sent Ctrl+Shift+V");
    }

    Ok(())
}

/// Place a UTF-16 string on the Windows clipboard as CF_UNICODETEXT.
fn set_clipboard_text(text: &str) -> Result<()> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let byte_len = utf16.len() * std::mem::size_of::<u16>();

    unsafe {
        OpenClipboard(HWND(std::ptr::null_mut())).ok().context("OpenClipboard failed")?;

        // Ensure clipboard is closed even if a later step fails.
        let result = (|| -> Result<()> {
            EmptyClipboard().ok().context("EmptyClipboard failed")?;

            let hmem = GlobalAlloc(GMEM_MOVEABLE, byte_len).context("GlobalAlloc failed")?;
            if hmem.0.is_null() {
                anyhow::bail!("GlobalAlloc returned null");
            }

            let dst = GlobalLock(hmem);
            if dst.is_null() {
                anyhow::bail!("GlobalLock failed");
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr() as *const u8, dst as *mut u8, byte_len);
            // GlobalUnlock returns 0 with last-error = NO_ERROR on success when lock count reaches 0; ignore.
            let _ = GlobalUnlock(hmem);

            // After SetClipboardData succeeds, the system owns hmem — do not free it.
            SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0))
                .context("SetClipboardData failed")?;
            Ok(())
        })();

        let _ = CloseClipboard();
        result
    }
}

/// Send Ctrl+Shift+V via SendInput (paste-without-formatting in most modern apps).
fn send_paste_plain() -> Result<()> {
    let v_key = VIRTUAL_KEY(b'V' as u16);
    let inputs = [
        make_input(VK_CONTROL, false),
        make_input(VK_SHIFT, false),
        make_input(v_key, false),
        make_input(v_key, true),
        make_input(VK_SHIFT, true),
        make_input(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if (sent as usize) != inputs.len() {
        anyhow::bail!("SendInput dispatched {sent}/{} events for Ctrl+Shift+V", inputs.len());
    }
    Ok(())
}

fn make_input(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if key_up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_mode_parses() {
        assert_eq!(ShellOutput::from_str("none").unwrap(), ShellOutput::None);
        assert_eq!(ShellOutput::from_str("clipboard").unwrap(), ShellOutput::Clipboard);
        assert_eq!(ShellOutput::from_str("paste").unwrap(), ShellOutput::Paste);
        assert!(ShellOutput::from_str("bogus").is_err());
    }
}
