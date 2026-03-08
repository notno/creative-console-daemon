use std::collections::HashMap;
use std::time::Duration;

use crate::config::WebhookPollConfig;

/// Polls webhook endpoints and returns button active states.
pub struct WebhookPoller {
    client: reqwest::Client,
}

impl WebhookPoller {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("Failed to create HTTP client for webhook polling");
        Self { client }
    }

    /// Poll a single webhook endpoint and return a map of button_id -> active state.
    pub async fn poll(&self, config: &WebhookPollConfig) -> HashMap<u8, bool> {
        let mut states = HashMap::new();

        let resp = match self.client.get(&config.url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(url = %config.url, error = %e, "Webhook poll failed");
                return states;
            }
        };

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!(url = %config.url, error = %e, "Webhook poll: invalid JSON");
                return states;
            }
        };

        for (&button_id, json_path) in &config.buttons {
            let value = resolve_json_path(&json, json_path);
            let active = match value {
                Some(serde_json::Value::Bool(b)) => *b,
                Some(serde_json::Value::String(s)) => s == "true" || s == "1",
                Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
                _ => false,
            };
            states.insert(button_id, active);
        }

        states
    }
}

/// Resolve a dot-separated JSON path like "slots.1.spotlit" against a JSON value.
fn resolve_json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = if let Some(obj) = current.as_object() {
            obj.get(segment)?
        } else if let Some(arr) = current.as_array() {
            let idx: usize = segment.parse().ok()?;
            arr.get(idx)?
        } else {
            return None;
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_nested_path() {
        let json: serde_json::Value = serde_json::json!({
            "slots": {
                "1": { "spotlit": true, "character": "Bessa" },
                "2": { "spotlit": false, "character": "Theron" }
            },
            "mode": "solo"
        });

        assert_eq!(resolve_json_path(&json, "slots.1.spotlit"), Some(&serde_json::json!(true)));
        assert_eq!(resolve_json_path(&json, "slots.2.spotlit"), Some(&serde_json::json!(false)));
        assert_eq!(resolve_json_path(&json, "mode"), Some(&serde_json::json!("solo")));
        assert_eq!(resolve_json_path(&json, "nonexistent"), None);
    }

    #[test]
    fn resolve_array_path() {
        let json: serde_json::Value = serde_json::json!({
            "items": [true, false, true]
        });

        assert_eq!(resolve_json_path(&json, "items.0"), Some(&serde_json::json!(true)));
        assert_eq!(resolve_json_path(&json, "items.1"), Some(&serde_json::json!(false)));
    }
}
