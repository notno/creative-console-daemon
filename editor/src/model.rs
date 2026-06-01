//! Editable in-memory model of the config and a deterministic TOML emitter.
//!
//! Loading reuses the daemon's real `Config` (parse + validate); editing happens
//! on this flat model; saving regenerates a canonical TOML file. Hand-written
//! comments in the source file are dropped on save — the editor owns the file.

use creative_console_daemon::config::{Action, Config, DeviceType};

const DEFAULT_VID: u16 = 0x046D;
const DEFAULT_PID: u16 = 0xC354;
const DEFAULT_USAGE_PAGE: u16 = 0xFF43;
const DEFAULT_FONT_SCALE: u32 = 2;
const WHITE: [u8; 3] = [255, 255, 255];
const BLACK: [u8; 3] = [0, 0, 0];

pub const MEDIA_KEYS: &[&str] =
    &["play_pause", "volume_up", "volume_down", "mute", "next_track", "prev_track"];
pub const SHELL_OUTPUTS: &[&str] = &["none", "clipboard", "paste"];
pub const HTTP_METHODS: &[&str] = &["POST", "GET", "PUT", "PATCH", "DELETE"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionKind {
    Obs,
    Webhook,
    Media,
    Hotkey,
    Shell,
}

impl ActionKind {
    pub const ALL: &'static [ActionKind] = &[
        ActionKind::Obs,
        ActionKind::Webhook,
        ActionKind::Media,
        ActionKind::Hotkey,
        ActionKind::Shell,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ActionKind::Obs => "OBS",
            ActionKind::Webhook => "Webhook",
            ActionKind::Media => "Media key",
            ActionKind::Hotkey => "Hotkey",
            ActionKind::Shell => "Shell",
        }
    }
}

/// All action fields live flat on the button so switching `kind` never loses
/// data the user already typed for another action type.
#[derive(Clone)]
pub struct ButtonEdit {
    pub id: u8,
    pub page: u16,

    pub label: String,
    pub icon: String,
    pub fg: [u8; 3],
    pub bg: [u8; 3],

    pub has_active: bool,
    pub active_label: String,
    pub active_icon: String,
    pub active_fg: [u8; 3],
    pub active_bg: [u8; 3],

    pub font_scale: u32,

    pub kind: ActionKind,
    // OBS
    pub obs_command: String,
    pub obs_params: Vec<(String, String)>,
    // Webhook
    pub wh_method: String,
    pub wh_url: String,
    pub wh_body: String,
    pub wh_headers: Vec<(String, String)>,
    pub wh_release_url: String,
    // Media
    pub media_key: String,
    // Hotkey
    pub hotkey_keys: Vec<String>,
    pub hotkey_hold: bool,
    // Shell
    pub shell_cmd: String,
    pub shell_args: Vec<String>,
    pub shell_output: String,
    pub shell_trim: bool,
}

impl ButtonEdit {
    pub fn new(id: u8, page: u16) -> Self {
        Self {
            id,
            page,
            label: String::new(),
            icon: String::new(),
            fg: WHITE,
            bg: BLACK,
            has_active: false,
            active_label: String::new(),
            active_icon: String::new(),
            active_fg: WHITE,
            active_bg: BLACK,
            font_scale: DEFAULT_FONT_SCALE,
            kind: ActionKind::Media,
            obs_command: String::new(),
            obs_params: Vec::new(),
            wh_method: "POST".into(),
            wh_url: String::new(),
            wh_body: String::new(),
            wh_headers: Vec::new(),
            wh_release_url: String::new(),
            media_key: "play_pause".into(),
            hotkey_keys: Vec::new(),
            hotkey_hold: false,
            shell_cmd: String::new(),
            shell_args: Vec::new(),
            shell_output: "paste".into(),
            shell_trim: true,
        }
    }
}

#[derive(Clone)]
pub struct ObsEdit {
    pub host: String,
    pub port: u16,
    pub password: String,
}

#[derive(Clone)]
pub struct WebhookPollEdit {
    pub url: String,
    pub interval_secs: u64,
    pub buttons: Vec<(u8, String)>,
}

#[derive(Clone)]
pub struct EditModel {
    pub device_type: DeviceType,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: Option<u16>,
    pub serial: String,

    pub obs_enabled: bool,
    pub obs: ObsEdit,

    pub buttons: Vec<ButtonEdit>,
    pub webhook_poll: Vec<WebhookPollEdit>,
}

impl EditModel {
    pub fn max_lcd_id(&self) -> u8 {
        match self.device_type {
            DeviceType::MxCreative => 9,
            DeviceType::StreamdeckXl => 32,
        }
    }

    pub fn page_count(&self) -> u16 {
        self.buttons.iter().map(|b| b.page).max().unwrap_or(1)
    }

    pub fn from_config(c: &Config) -> Self {
        let buttons = c
            .button
            .iter()
            .map(|b| {
                let mut e = ButtonEdit::new(b.id, b.page);
                e.label = b.label.clone().unwrap_or_default();
                e.icon = b.icon.clone().unwrap_or_default();
                e.fg = b.fg.unwrap_or(WHITE);
                e.bg = b.bg.unwrap_or(BLACK);
                e.font_scale = b.font_scale.unwrap_or(DEFAULT_FONT_SCALE);

                let has_active = b.active_label.is_some()
                    || b.active_icon.is_some()
                    || b.active_fg.is_some()
                    || b.active_bg.is_some();
                e.has_active = has_active;
                e.active_label = b.active_label.clone().unwrap_or_default();
                e.active_icon = b.active_icon.clone().unwrap_or_default();
                e.active_fg = b.active_fg.unwrap_or(e.fg);
                e.active_bg = b.active_bg.unwrap_or(e.bg);

                match &b.action {
                    Action::Obs { command, params } => {
                        e.kind = ActionKind::Obs;
                        e.obs_command = command.clone();
                        let mut p: Vec<(String, String)> = params
                            .iter()
                            .map(|(k, v)| (k.clone(), value_to_edit_string(v)))
                            .collect();
                        p.sort_by(|a, b| a.0.cmp(&b.0));
                        e.obs_params = p;
                    }
                    Action::Webhook { method, url, body, headers, release_url } => {
                        e.kind = ActionKind::Webhook;
                        e.wh_method = method.clone();
                        e.wh_url = url.clone();
                        e.wh_body = body.clone().unwrap_or_default();
                        let mut h: Vec<(String, String)> =
                            headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        h.sort_by(|a, b| a.0.cmp(&b.0));
                        e.wh_headers = h;
                        e.wh_release_url = release_url.clone().unwrap_or_default();
                    }
                    Action::Media { key } => {
                        e.kind = ActionKind::Media;
                        e.media_key = key.clone();
                    }
                    Action::Hotkey { keys, hold } => {
                        e.kind = ActionKind::Hotkey;
                        e.hotkey_keys = keys.clone();
                        e.hotkey_hold = *hold;
                    }
                    Action::Shell { cmd, args, output, trim } => {
                        e.kind = ActionKind::Shell;
                        e.shell_cmd = cmd.clone();
                        e.shell_args = args.clone();
                        e.shell_output = output.clone();
                        e.shell_trim = *trim;
                    }
                }
                e
            })
            .collect();

        let webhook_poll = c
            .webhook_poll
            .iter()
            .map(|w| {
                let mut buttons: Vec<(u8, String)> =
                    w.buttons.iter().map(|(k, v)| (*k, v.clone())).collect();
                buttons.sort_by_key(|(k, _)| *k);
                WebhookPollEdit { url: w.url.clone(), interval_secs: w.interval_secs, buttons }
            })
            .collect();

        Self {
            device_type: c.device.device_type,
            vendor_id: c.device.vendor_id,
            product_id: c.device.product_id,
            usage_page: c.device.usage_page,
            usage: c.device.usage,
            serial: c.device.serial.clone().unwrap_or_default(),
            obs_enabled: c.obs.is_some(),
            obs: c
                .obs
                .as_ref()
                .map(|o| ObsEdit {
                    host: o.host.clone(),
                    port: o.port,
                    password: o.password.clone().unwrap_or_default(),
                })
                .unwrap_or(ObsEdit { host: "localhost".into(), port: 4455, password: String::new() }),
            buttons,
            webhook_poll,
        }
    }

    /// Emit canonical TOML. Deterministic ordering for clean diffs.
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        out.push_str("# Managed by the Creative Console editor. Hand edits to comments\n");
        out.push_str("# and formatting are not preserved when saved from the editor.\n\n");

        // --- [device] ---
        out.push_str("[device]\n");
        let dt = match self.device_type {
            DeviceType::MxCreative => "mx_creative",
            DeviceType::StreamdeckXl => "streamdeck_xl",
        };
        kv(&mut out, "device_type", &tstr(dt));
        if self.vendor_id != DEFAULT_VID {
            kv(&mut out, "vendor_id", &format!("0x{:04X}", self.vendor_id));
        }
        if self.product_id != DEFAULT_PID {
            kv(&mut out, "product_id", &format!("0x{:04X}", self.product_id));
        }
        if self.usage_page != DEFAULT_USAGE_PAGE {
            kv(&mut out, "usage_page", &format!("0x{:04X}", self.usage_page));
        }
        if let Some(u) = self.usage {
            kv(&mut out, "usage", &format!("0x{u:04X}"));
        }
        if !self.serial.is_empty() {
            kv(&mut out, "serial", &tstr(&self.serial));
        }
        out.push('\n');

        // --- [obs] ---
        if self.obs_enabled {
            out.push_str("[obs]\n");
            kv(&mut out, "host", &tstr(&self.obs.host));
            kv(&mut out, "port", &self.obs.port.to_string());
            if !self.obs.password.is_empty() {
                kv(&mut out, "password", &tstr(&self.obs.password));
            }
            out.push('\n');
        }

        // --- [[button]] --- sorted by page then id for stable output.
        let mut buttons: Vec<&ButtonEdit> = self.buttons.iter().collect();
        buttons.sort_by_key(|b| (b.page, b.id));
        for b in buttons {
            out.push_str("[[button]]\n");
            kv(&mut out, "id", &b.id.to_string());
            if b.page != 1 {
                kv(&mut out, "page", &b.page.to_string());
            }
            if !b.label.is_empty() {
                kv(&mut out, "label", &tstr(&b.label));
            }
            if !b.icon.is_empty() {
                kv(&mut out, "icon", &tstr(&b.icon));
            }
            if b.fg != WHITE {
                kv(&mut out, "fg", &trgb(b.fg));
            }
            if b.bg != BLACK {
                kv(&mut out, "bg", &trgb(b.bg));
            }
            if b.font_scale != DEFAULT_FONT_SCALE {
                kv(&mut out, "font_scale", &b.font_scale.to_string());
            }
            if b.has_active {
                if !b.active_label.is_empty() {
                    kv(&mut out, "active_label", &tstr(&b.active_label));
                }
                if !b.active_icon.is_empty() {
                    kv(&mut out, "active_icon", &tstr(&b.active_icon));
                }
                kv(&mut out, "active_fg", &trgb(b.active_fg));
                kv(&mut out, "active_bg", &trgb(b.active_bg));
            }

            match b.kind {
                ActionKind::Obs => {
                    kv(&mut out, "action", &tstr("obs"));
                    kv(&mut out, "command", &tstr(&b.obs_command));
                    if !b.obs_params.is_empty() {
                        let inner: Vec<String> = b
                            .obs_params
                            .iter()
                            .filter(|(k, _)| !k.is_empty())
                            .map(|(k, v)| format!("{} = {}", bare_or_quoted_key(k), edit_string_to_value(v)))
                            .collect();
                        kv(&mut out, "params", &format!("{{ {} }}", inner.join(", ")));
                    }
                }
                ActionKind::Webhook => {
                    kv(&mut out, "action", &tstr("webhook"));
                    kv(&mut out, "method", &tstr(&b.wh_method));
                    kv(&mut out, "url", &tstr(&b.wh_url));
                    if !b.wh_body.is_empty() {
                        kv(&mut out, "body", &tstr(&b.wh_body));
                    }
                    if !b.wh_release_url.is_empty() {
                        kv(&mut out, "release_url", &tstr(&b.wh_release_url));
                    }
                    if !b.wh_headers.is_empty() {
                        let inner: Vec<String> = b
                            .wh_headers
                            .iter()
                            .filter(|(k, _)| !k.is_empty())
                            .map(|(k, v)| format!("{} = {}", bare_or_quoted_key(k), tstr(v)))
                            .collect();
                        kv(&mut out, "headers", &format!("{{ {} }}", inner.join(", ")));
                    }
                }
                ActionKind::Media => {
                    kv(&mut out, "action", &tstr("media"));
                    kv(&mut out, "key", &tstr(&b.media_key));
                }
                ActionKind::Hotkey => {
                    kv(&mut out, "action", &tstr("hotkey"));
                    kv(&mut out, "keys", &tstr_array(&b.hotkey_keys));
                    if b.hotkey_hold {
                        kv(&mut out, "hold", "true");
                    }
                }
                ActionKind::Shell => {
                    kv(&mut out, "action", &tstr("shell"));
                    kv(&mut out, "cmd", &tstr(&b.shell_cmd));
                    if !b.shell_args.is_empty() {
                        kv(&mut out, "args", &tstr_array(&b.shell_args));
                    }
                    kv(&mut out, "output", &tstr(&b.shell_output));
                    if !b.shell_trim {
                        kv(&mut out, "trim", "false");
                    }
                }
            }
            out.push('\n');
        }

        // --- [[webhook_poll]] ---
        for w in &self.webhook_poll {
            out.push_str("[[webhook_poll]]\n");
            kv(&mut out, "url", &tstr(&w.url));
            kv(&mut out, "interval_secs", &w.interval_secs.to_string());
            if !w.buttons.is_empty() {
                let inner: Vec<String> = w
                    .buttons
                    .iter()
                    .map(|(id, path)| format!("{} = {}", tstr(&id.to_string()), tstr(path)))
                    .collect();
                kv(&mut out, "buttons", &format!("{{ {} }}", inner.join(", ")));
            }
            out.push('\n');
        }

        out
    }
}

fn kv(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(value);
    out.push('\n');
}

/// Render a string as a TOML basic string (correct escaping via toml::Value).
fn tstr(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn tstr_array(items: &[String]) -> String {
    let v = toml::Value::Array(items.iter().map(|s| toml::Value::String(s.clone())).collect());
    v.to_string()
}

fn trgb(c: [u8; 3]) -> String {
    let v = toml::Value::Array(vec![
        toml::Value::Integer(c[0] as i64),
        toml::Value::Integer(c[1] as i64),
        toml::Value::Integer(c[2] as i64),
    ]);
    v.to_string()
}

/// Bare key if it's a valid TOML bare key, else a quoted key.
fn bare_or_quoted_key(k: &str) -> String {
    let bare = !k.is_empty()
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        k.to_string()
    } else {
        tstr(k)
    }
}

/// Display a toml::Value as an editable string (raw text for strings).
fn value_to_edit_string(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Parse editable text back into a TOML value literal, inferring bool/int/float
/// and falling back to a quoted string.
fn edit_string_to_value(s: &str) -> String {
    if s == "true" || s == "false" {
        return s.to_string();
    }
    if let Ok(i) = s.parse::<i64>() {
        return i.to_string();
    }
    if let Ok(f) = s.parse::<f64>() {
        if f.is_finite() {
            return f.to_string();
        }
    }
    tstr(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Load a real config, emit canonical TOML, re-parse, and confirm the
    /// round-trip preserves button count and survives validation. Exercises
    /// the shell config's nested quotes/backslashes and multi-line labels.
    fn roundtrip(path: &str) {
        let original = Config::load(std::path::Path::new(path)).expect("load original");
        let model = EditModel::from_config(&original);
        let toml = model.to_toml();
        let reparsed = Config::parse(&toml)
            .unwrap_or_else(|e| panic!("re-parse of emitted TOML failed for {path}: {e:#}\n---\n{toml}"));
        assert_eq!(
            original.button.len(),
            reparsed.button.len(),
            "button count changed for {path}"
        );
        assert_eq!(original.page_count(), reparsed.page_count(), "page count changed for {path}");
    }

    #[test]
    fn roundtrip_ctrl_win() {
        roundtrip("../config.ctrl-win.toml");
    }

    #[test]
    fn roundtrip_example() {
        roundtrip("../config.example.toml");
    }

    #[test]
    fn roundtrip_streamdeck() {
        roundtrip("../config.streamdeck.toml");
    }

    #[test]
    fn shell_args_with_quotes_and_backslashes_survive() {
        let original = Config::load(std::path::Path::new("../config.ctrl-win.toml")).unwrap();
        let model = EditModel::from_config(&original);
        let toml = model.to_toml();
        let reparsed = Config::parse(&toml).unwrap();

        // Find the "Paste Latest Shot" shell button on page 2 and compare its args.
        let find_shell = |c: &Config| -> Vec<String> {
            c.button
                .iter()
                .find_map(|b| match &b.action {
                    Action::Shell { args, .. } if !args.is_empty() => Some(args.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        };
        assert_eq!(find_shell(&original), find_shell(&reparsed), "shell args corrupted on round-trip");
    }
}
