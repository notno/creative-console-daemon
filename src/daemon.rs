use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::actions;
use crate::actions::obs::ObsClient;
use crate::actions::webhook::WebhookClient;
use crate::config::Config;
use crate::hid::device;
use crate::hid::lcd::LcdWriter;
use crate::hid::protocol::ButtonEvent;

/// Debounce interval: ignore repeated Down events for the same button within this window.
const DEBOUNCE_MS: u64 = 100;

/// Run the main daemon event loop.
/// Returns the process exit code (0 = clean shutdown, 2 = device disconnected).
pub async fn run(config: Config, shutdown: Arc<AtomicBool>, dry_run: bool) -> i32 {
    // Open device and spawn HID reader thread
    let rx = match device::spawn_reader(&config.device, shutdown.clone()) {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!("Failed to start HID reader: {}", e);
            return 1;
        }
    };

    // Initialize LCD button images
    init_lcd_buttons(&config);

    // Initialize action clients
    let mut obs_client = ObsClient::new(config.obs.clone());
    let webhook_client = WebhookClient::new();

    // Debounce tracking: last Down time per button config_id
    let mut last_down: std::collections::HashMap<u8, Instant> = std::collections::HashMap::new();

    // Event loop
    run_event_loop(rx, &config, &mut obs_client, &webhook_client, &mut last_down, &shutdown, dry_run).await
}

/// Initialize LCD button images from config (labels, icons, or auto-generated).
fn init_lcd_buttons(config: &Config) {
    let api = match hidapi::HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            tracing::warn!("Failed to init HID API for LCD: {e}");
            return;
        }
    };

    let lcd = match LcdWriter::open(
        &api,
        config.device.vendor_id,
        config.device.product_id,
        config.device.usage_page,
    ) {
        Ok(lcd) => lcd,
        Err(e) => {
            tracing::warn!("LCD output not available: {e}");
            return;
        }
    };

    for mapping in &config.button {
        if mapping.id < 1 || mapping.id > 9 {
            continue; // Only LCD buttons have screens
        }

        let fg = mapping.fg.unwrap_or([255, 255, 255]);
        let bg = mapping.bg.unwrap_or([0, 0, 0]);

        let result = if let Some(icon_path) = &mapping.icon {
            lcd.write_button_file(mapping.id, std::path::Path::new(icon_path))
        } else if let Some(label) = &mapping.label {
            lcd.write_button_label(mapping.id, label, fg, bg)
        } else {
            // Auto-generate label from action type
            let auto_label = match &mapping.action {
                crate::config::Action::Obs { command, .. } => {
                    shorten_obs_command(command)
                }
                crate::config::Action::Media { key } => key.replace('_', " "),
                crate::config::Action::Webhook { method, .. } => {
                    format!("{method}")
                }
            };
            lcd.write_button_label(mapping.id, &auto_label, fg, bg)
        };

        if let Err(e) = result {
            tracing::warn!(button = mapping.id, "Failed to set LCD image: {e}");
        } else {
            tracing::debug!(button = mapping.id, "LCD button image set");
        }
    }
}

/// Shorten OBS command names for LCD display.
fn shorten_obs_command(cmd: &str) -> String {
    match cmd {
        "SetCurrentProgramScene" => "SCENE".to_string(),
        "ToggleRecord" => "REC".to_string(),
        "StartRecord" => "REC ON".to_string(),
        "StopRecord" => "REC OFF".to_string(),
        "ToggleInputMute" => "MUTE".to_string(),
        other => {
            // Truncate long names
            if other.len() > 10 {
                other[..10].to_string()
            } else {
                other.to_string()
            }
        }
    }
}

async fn run_event_loop(
    mut rx: mpsc::Receiver<ButtonEvent>,
    config: &Config,
    obs_client: &mut ObsClient,
    webhook_client: &WebhookClient,
    last_down: &mut std::collections::HashMap<u8, Instant>,
    shutdown: &Arc<AtomicBool>,
    dry_run: bool,
) -> i32 {
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(ButtonEvent::Down(btn)) => {
                        let config_id = btn.to_config_id();

                        // Debounce check
                        let now = Instant::now();
                        if let Some(&last) = last_down.get(&config_id) {
                            if now.duration_since(last) < Duration::from_millis(DEBOUNCE_MS) {
                                tracing::trace!(button = %btn, "Debounced");
                                continue;
                            }
                        }
                        last_down.insert(config_id, now);

                        tracing::info!(button = %btn, config_id, "Button pressed");

                        if dry_run {
                            tracing::info!(button = %btn, "Dry run: skipping action dispatch");
                        } else {
                            actions::dispatch(btn, config, obs_client, webhook_client).await;
                        }
                    }
                    Some(ButtonEvent::Up(btn)) => {
                        tracing::debug!(button = %btn, "Button released");
                        // MVP: no action on release
                    }
                    None => {
                        // Channel closed = HID thread exited (device disconnected or error)
                        if shutdown.load(Ordering::Relaxed) {
                            tracing::info!("Shutting down (user requested)");
                            return 0;
                        } else {
                            tracing::error!("Device disconnected");
                            return 2;
                        }
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl+C received, shutting down...");
                shutdown.store(true, Ordering::Relaxed);
                // Give HID thread time to exit
                tokio::time::sleep(Duration::from_millis(600)).await;
                return 0;
            }
        }
    }
}
