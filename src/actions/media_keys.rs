use anyhow::Result;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
    VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
};

/// Map a config key name to a Windows virtual key code.
fn map_key(key: &str) -> Result<VIRTUAL_KEY> {
    match key {
        "play_pause" => Ok(VK_MEDIA_PLAY_PAUSE),
        "volume_up" => Ok(VK_VOLUME_UP),
        "volume_down" => Ok(VK_VOLUME_DOWN),
        "mute" => Ok(VK_VOLUME_MUTE),
        "next_track" => Ok(VK_MEDIA_NEXT_TRACK),
        "prev_track" => Ok(VK_MEDIA_PREV_TRACK),
        _ => anyhow::bail!("Unknown media key: {key}"),
    }
}

/// Send a media key press (key down + key up).
pub fn send_media_key(key: &str) -> Result<()> {
    let vk = map_key(key)?;

    let key_down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: KEYBD_EVENT_FLAGS(0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    let key_up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    let inputs = [key_down, key_up];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };

    if sent == 0 {
        anyhow::bail!("SendInput failed for media key '{key}'");
    }

    tracing::info!(key, "Sent media key");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_key_mappings() {
        assert!(map_key("play_pause").is_ok());
        assert!(map_key("volume_up").is_ok());
        assert!(map_key("volume_down").is_ok());
        assert!(map_key("mute").is_ok());
        assert!(map_key("next_track").is_ok());
        assert!(map_key("prev_track").is_ok());
    }

    #[test]
    fn invalid_key_mapping() {
        assert!(map_key("invalid").is_err());
    }
}
