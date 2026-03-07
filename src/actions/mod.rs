pub mod media_keys;
pub mod obs;
pub mod webhook;

use crate::config::{Action, Config};
use crate::hid::protocol::ButtonId;

/// Dispatch an action for a button press.
pub async fn dispatch(button_id: ButtonId, config: &Config, obs: &mut obs::ObsClient, webhook: &webhook::WebhookClient) {
    let config_id = button_id.to_config_id();

    let mapping = config.button.iter().find(|b| b.id == config_id);
    let mapping = match mapping {
        Some(m) => m,
        None => {
            tracing::debug!(button = %button_id, config_id, "No mapping configured for button");
            return;
        }
    };

    tracing::info!(button = %button_id, config_id, "Dispatching action");

    match &mapping.action {
        Action::Obs { command, params } => {
            if let Err(e) = obs.execute(command, params).await {
                tracing::warn!(command, error = %e, "OBS action failed");
            }
        }
        Action::Webhook { method, url, body, headers } => {
            if let Err(e) = webhook.send(method, url, body.as_deref(), headers).await {
                tracing::warn!(url, error = %e, "Webhook action failed");
            }
        }
        Action::Media { key } => {
            if let Err(e) = media_keys::send_media_key(key) {
                tracing::warn!(key, error = %e, "Media key action failed");
            }
        }
    }
}
