use anyhow::{Context, Result};
use obws::requests::inputs::InputId;
use std::collections::HashMap;

use crate::config::ObsConfig;

/// Wrapper around obws::Client that handles lazy connection and reconnection.
pub struct ObsClient {
    config: Option<ObsConfig>,
    client: Option<obws::Client>,
}

impl ObsClient {
    pub fn new(config: Option<ObsConfig>) -> Self {
        Self {
            config,
            client: None,
        }
    }

    /// Ensure we have a connected client. Creates one if needed.
    async fn ensure_connected(&mut self) -> Result<&obws::Client> {
        if self.client.is_some() {
            return Ok(self.client.as_ref().unwrap());
        }

        let config = self.config.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OBS action requested but no [obs] section in config")
        })?;

        let connect_result = obws::Client::connect(
            &config.host,
            config.port,
            config.password.as_deref(),
        )
        .await;

        match connect_result {
            Ok(client) => {
                tracing::info!("Connected to OBS WebSocket at {}:{}", config.host, config.port);
                self.client = Some(client);
                Ok(self.client.as_ref().unwrap())
            }
            Err(e) => {
                tracing::warn!("Failed to connect to OBS: {}. Will retry on next action.", e);
                Err(anyhow::anyhow!("OBS connection failed: {e}"))
            }
        }
    }

    /// Execute an OBS command. On failure, drops the client for reconnection on next call.
    pub async fn execute(&mut self, command: &str, params: &HashMap<String, toml::Value>) -> Result<()> {
        let result = self.execute_inner(command, params).await;
        if result.is_err() {
            // Drop client so next call attempts reconnection
            self.client = None;
        }
        result
    }

    async fn execute_inner(&mut self, command: &str, params: &HashMap<String, toml::Value>) -> Result<()> {
        let client = self.ensure_connected().await?;

        match command {
            "SetCurrentProgramScene" => {
                let scene_name = params
                    .get("sceneName")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("SetCurrentProgramScene requires 'sceneName' param"))?;
                client
                    .scenes()
                    .set_current_program_scene(scene_name)
                    .await
                    .context("Failed to set program scene")?;
                tracing::info!(scene = scene_name, "Switched OBS scene");
            }
            "StartRecord" => {
                client.recording().start().await.context("Failed to start recording")?;
                tracing::info!("Started OBS recording");
            }
            "StopRecord" => {
                client.recording().stop().await.context("Failed to stop recording")?;
                tracing::info!("Stopped OBS recording");
            }
            "ToggleRecord" => {
                client.recording().toggle().await.context("Failed to toggle recording")?;
                tracing::info!("Toggled OBS recording");
            }
            "ToggleInputMute" => {
                let input_name = params
                    .get("inputName")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("ToggleInputMute requires 'inputName' param"))?;
                let input_id = InputId::Name(input_name);
                let current = client
                    .inputs()
                    .muted(input_id)
                    .await
                    .context("Failed to get mute state")?;
                let input_id = InputId::Name(input_name);
                client
                    .inputs()
                    .set_muted(input_id, !current)
                    .await
                    .context("Failed to toggle mute")?;
                tracing::info!(input = input_name, muted = !current, "Toggled OBS input mute");
            }
            _ => {
                tracing::warn!(command, "Unknown OBS command");
                anyhow::bail!("Unknown OBS command: {command}");
            }
        }

        Ok(())
    }
}
