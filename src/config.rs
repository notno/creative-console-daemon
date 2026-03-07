use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub device: DeviceConfig,
    pub obs: Option<ObsConfig>,
    #[serde(default)]
    pub button: Vec<ButtonMapping>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceConfig {
    #[serde(default = "default_vid")]
    pub vendor_id: u16,
    #[serde(default = "default_pid")]
    pub product_id: u16,
    #[serde(default = "default_usage_page")]
    pub usage_page: u16,
    /// Optional usage filter — if set, also match on HID usage within the usage page.
    pub usage: Option<u16>,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            vendor_id: default_vid(),
            product_id: default_pid(),
            usage_page: default_usage_page(),
            usage: None,
        }
    }
}

fn default_vid() -> u16 {
    0x046D
}
fn default_pid() -> u16 {
    0xC354
}
fn default_usage_page() -> u16 {
    0xFF43
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObsConfig {
    #[serde(default = "default_obs_host")]
    pub host: String,
    #[serde(default = "default_obs_port")]
    pub port: u16,
    pub password: Option<String>,
}

fn default_obs_host() -> String {
    "localhost".to_string()
}
fn default_obs_port() -> u16 {
    4455
}

#[derive(Debug, Deserialize)]
pub struct ButtonMapping {
    pub id: u8,
    /// Page number (default: 1). Same button ID can appear on different pages.
    #[serde(default = "default_page")]
    pub page: u16,
    /// Optional label text to display on the LCD button (inactive/default state).
    pub label: Option<String>,
    /// Optional image file path to display on the LCD button.
    pub icon: Option<String>,
    /// Label foreground color as [R, G, B] (default: white).
    pub fg: Option<[u8; 3]>,
    /// Label background color as [R, G, B] (default: black).
    pub bg: Option<[u8; 3]>,
    /// Label text when action is active (e.g., recording in progress).
    pub active_label: Option<String>,
    /// Foreground color when active.
    pub active_fg: Option<[u8; 3]>,
    /// Background color when active.
    pub active_bg: Option<[u8; 3]>,
    /// Icon file when active.
    pub active_icon: Option<String>,
    #[serde(flatten)]
    pub action: Action,
}

fn default_page() -> u16 {
    1
}

impl Config {
    /// Get the total number of pages defined in the config.
    pub fn page_count(&self) -> u16 {
        self.button.iter().map(|b| b.page).max().unwrap_or(1)
    }

    /// Get button mappings for a specific page (LCD buttons only, ids 1-9).
    pub fn buttons_on_page(&self, page: u16) -> Vec<&ButtonMapping> {
        self.button
            .iter()
            .filter(|b| b.page == page && b.id >= 1 && b.id <= 9)
            .collect()
    }

    /// Find the mapping for a button ID on a given page.
    pub fn find_button(&self, page: u16, config_id: u8) -> Option<&ButtonMapping> {
        self.button
            .iter()
            .find(|b| b.page == page && b.id == config_id)
    }

    /// Check if PageLeft (10) or PageRight (11) have an explicit action mapping.
    pub fn has_page_button_action(&self, config_id: u8) -> bool {
        self.button.iter().any(|b| b.id == config_id)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read config: {}", path.display()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| format!("Failed to parse config: {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        for mapping in &self.button {
            if mapping.id < 1 || mapping.id > 11 {
                anyhow::bail!(
                    "Button ID {} is out of range. Valid range: 1-11 (1-9 for LCD buttons, 10=PageLeft, 11=PageRight)",
                    mapping.id
                );
            }
            if let Action::Media { key } = &mapping.action {
                validate_media_key(key)?;
            }
            if let Action::Webhook { url, .. } = &mapping.action {
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    anyhow::bail!("Webhook URL must start with http:// or https://: {url}");
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Action {
    Obs {
        command: String,
        #[serde(default)]
        params: HashMap<String, toml::Value>,
    },
    Webhook {
        #[serde(default = "default_method")]
        method: String,
        url: String,
        body: Option<String>,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    Media {
        key: String,
    },
}

fn default_method() -> String {
    "POST".to_string()
}

const VALID_MEDIA_KEYS: &[&str] = &[
    "play_pause",
    "volume_up",
    "volume_down",
    "mute",
    "next_track",
    "prev_track",
];

fn validate_media_key(key: &str) -> Result<()> {
    if !VALID_MEDIA_KEYS.contains(&key) {
        anyhow::bail!(
            "Unknown media key '{}'. Valid keys: {}",
            key,
            VALID_MEDIA_KEYS.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_config() {
        let toml_str = r#"
[device]
vendor_id = 0x046D
product_id = 0xC354
usage_page = 0xFF00

[obs]
host = "localhost"
port = 4455

[[button]]
id = 1
action = "obs"
command = "SetCurrentProgramScene"
params = { sceneName = "Camera 1" }

[[button]]
id = 3
action = "media"
key = "play_pause"

[[button]]
id = 4
action = "webhook"
method = "POST"
url = "http://localhost:8080/api/trigger"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.device.vendor_id, 0x046D);
        assert_eq!(config.device.product_id, 0xC354);
        assert_eq!(config.button.len(), 3);
        config.validate().unwrap();
    }

    #[test]
    fn defaults_applied() {
        let toml_str = r#"
[[button]]
id = 1
action = "media"
key = "play_pause"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.device.vendor_id, 0x046D);
        assert_eq!(config.device.product_id, 0xC354);
        assert_eq!(config.device.usage_page, 0xFF43);
    }

    #[test]
    fn invalid_button_id() {
        let toml_str = r#"
[[button]]
id = 15
action = "media"
key = "play_pause"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn invalid_media_key() {
        let toml_str = r#"
[[button]]
id = 1
action = "media"
key = "invalid_key"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn invalid_webhook_url() {
        let toml_str = r#"
[[button]]
id = 1
action = "webhook"
url = "ftp://bad-protocol.com"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn unknown_action_type() {
        let toml_str = r#"
[[button]]
id = 1
action = "unknown"
"#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }
}
