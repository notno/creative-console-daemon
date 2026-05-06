use anyhow::{Context, Result};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_INSERT, VK_LCONTROL,
    VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_NEXT, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SPACE, VK_TAB, VK_UP,
};

pub fn parse_key(name: &str) -> Result<VIRTUAL_KEY> {
    let n = name.to_ascii_lowercase();
    let vk = match n.as_str() {
        "ctrl" | "control" | "lctrl" => VK_LCONTROL,
        "rctrl" => VK_RCONTROL,
        "shift" | "lshift" => VK_LSHIFT,
        "rshift" => VK_RSHIFT,
        "alt" | "lalt" => VK_LMENU,
        "ralt" => VK_RMENU,
        "win" | "lwin" => VK_LWIN,
        "rwin" => VK_RWIN,
        "space" => VK_SPACE,
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "esc" | "escape" => VK_ESCAPE,
        "backspace" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "home" => VK_HOME,
        "end" => VK_END,
        "pgup" | "pageup" => VK_PRIOR,
        "pgdn" | "pagedown" => VK_NEXT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "insert" | "ins" => VK_INSERT,
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_alphanumeric() => {
            VIRTUAL_KEY(s.chars().next().unwrap().to_ascii_uppercase() as u16)
        }
        s if s.starts_with('f') && s.len() >= 2 => {
            let num: u16 = s[1..]
                .parse()
                .with_context(|| format!("Unknown key '{name}'"))?;
            if !(1..=24).contains(&num) {
                anyhow::bail!("Function key out of range (F1-F24): {name}");
            }
            VIRTUAL_KEY(0x6F + num)
        }
        _ => anyhow::bail!("Unknown key '{name}'"),
    };
    Ok(vk)
}

pub fn send_hotkey(keys: &[String]) -> Result<()> {
    press_hotkey(keys)?;
    release_hotkey(keys)?;
    Ok(())
}

pub fn press_hotkey(keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        anyhow::bail!("Hotkey has no keys");
    }
    let vks: Vec<VIRTUAL_KEY> = keys.iter().map(|k| parse_key(k)).collect::<Result<_>>()?;
    let inputs: Vec<INPUT> = vks.iter().map(|&vk| make_input(vk, false)).collect();
    dispatch_inputs(&inputs)?;
    tracing::info!(?keys, "Pressed hotkey");
    Ok(())
}

pub fn release_hotkey(keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        anyhow::bail!("Hotkey has no keys");
    }
    let vks: Vec<VIRTUAL_KEY> = keys.iter().map(|k| parse_key(k)).collect::<Result<_>>()?;
    let inputs: Vec<INPUT> = vks.iter().rev().map(|&vk| make_input(vk, true)).collect();
    dispatch_inputs(&inputs)?;
    tracing::info!(?keys, "Released hotkey");
    Ok(())
}

fn dispatch_inputs(inputs: &[INPUT]) -> Result<()> {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if (sent as usize) != inputs.len() {
        anyhow::bail!("SendInput dispatched {sent}/{} events", inputs.len());
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
    fn parses_modifiers() {
        assert!(parse_key("ctrl").is_ok());
        assert!(parse_key("WIN").is_ok());
        assert!(parse_key("alt").is_ok());
        assert!(parse_key("shift").is_ok());
    }

    #[test]
    fn parses_letters_and_digits() {
        assert!(parse_key("a").is_ok());
        assert!(parse_key("Z").is_ok());
        assert!(parse_key("5").is_ok());
    }

    #[test]
    fn parses_function_keys() {
        assert!(parse_key("f1").is_ok());
        assert!(parse_key("F12").is_ok());
        assert!(parse_key("f24").is_ok());
        assert!(parse_key("f25").is_err());
    }

    #[test]
    fn rejects_unknown() {
        assert!(parse_key("nope").is_err());
        assert!(parse_key("").is_err());
    }
}
