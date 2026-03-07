use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::actions;
use crate::actions::obs::{ObsClient, ObsState};
use crate::actions::webhook::WebhookClient;
use crate::config::{Action, ButtonMapping, Config};
use crate::hid::device;
use crate::hid::lcd::LcdWriter;
use crate::hid::protocol::ButtonEvent;

/// Debounce interval: ignore repeated Down events for the same button within this window.
const DEBOUNCE_MS: u64 = 100;

/// How often to poll OBS for state changes (seconds).
const STATE_POLL_INTERVAL_SECS: u64 = 2;

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

    // Initialize LCD
    let lcd = open_lcd(&config);
    if let Some(ref lcd) = lcd {
        render_all_buttons(&config, lcd, &HashMap::new());
    }

    // Collect input names that need mute tracking
    let mute_inputs: Vec<String> = config
        .button
        .iter()
        .filter_map(|b| {
            if let Action::Obs { command, params } = &b.action {
                if command == "ToggleInputMute" {
                    return params.get("inputName").and_then(|v| v.as_str()).map(String::from);
                }
            }
            None
        })
        .collect();

    // Initialize action clients
    let mut obs_client = ObsClient::new(config.obs.clone());
    let webhook_client = WebhookClient::new();

    // Debounce tracking: last Down time per button config_id
    let mut last_down: HashMap<u8, Instant> = HashMap::new();

    // Track which buttons are currently in "active" state
    let mut button_active: HashMap<u8, bool> = HashMap::new();

    // Event loop
    run_event_loop(
        rx, &config, &mut obs_client, &webhook_client,
        &mut last_down, &shutdown, dry_run,
        lcd.as_ref(), &mute_inputs, &mut button_active,
    ).await
}

/// Open the LCD writer (returns None if not available).
fn open_lcd(config: &Config) -> Option<LcdWriter> {
    let api = hidapi::HidApi::new().ok()?;
    match LcdWriter::open(
        &api,
        config.device.vendor_id,
        config.device.product_id,
        config.device.usage_page,
    ) {
        Ok(lcd) => Some(lcd),
        Err(e) => {
            tracing::warn!("LCD output not available: {e}");
            None
        }
    }
}

/// Render a single button in its current state (active or inactive).
fn render_button(mapping: &ButtonMapping, lcd: &LcdWriter, active: bool) {
    if mapping.id < 1 || mapping.id > 9 {
        return;
    }

    let (label, icon, fg, bg) = if active {
        (
            mapping.active_label.as_deref().or(mapping.label.as_deref()),
            mapping.active_icon.as_deref().or(mapping.icon.as_deref()),
            mapping.active_fg.unwrap_or(mapping.fg.unwrap_or([255, 255, 255])),
            mapping.active_bg.unwrap_or(mapping.bg.unwrap_or([0, 0, 0])),
        )
    } else {
        (
            mapping.label.as_deref(),
            mapping.icon.as_deref(),
            mapping.fg.unwrap_or([255, 255, 255]),
            mapping.bg.unwrap_or([0, 0, 0]),
        )
    };

    let result = if let Some(icon_path) = icon {
        lcd.write_button_file(mapping.id, std::path::Path::new(icon_path))
    } else if let Some(label_text) = label {
        lcd.write_button_label(mapping.id, label_text, fg, bg)
    } else {
        let auto_label = match &mapping.action {
            Action::Obs { command, .. } => shorten_obs_command(command),
            Action::Media { key } => key.replace('_', " "),
            Action::Webhook { method, .. } => method.clone(),
        };
        lcd.write_button_label(mapping.id, &auto_label, fg, bg)
    };

    if let Err(e) = result {
        tracing::warn!(button = mapping.id, "Failed to set LCD image: {e}");
    }
}

/// Render all buttons with their current active states.
fn render_all_buttons(config: &Config, lcd: &LcdWriter, active_states: &HashMap<u8, bool>) {
    for mapping in &config.button {
        let active = active_states.get(&mapping.id).copied().unwrap_or(false);
        render_button(mapping, lcd, active);
    }
}

/// Determine if a button's action is currently "active" based on OBS state.
fn is_button_active(mapping: &ButtonMapping, obs_state: &ObsState) -> bool {
    match &mapping.action {
        Action::Obs { command, params } => match command.as_str() {
            "ToggleRecord" | "StartRecord" | "StopRecord" => obs_state.recording,
            "SetCurrentProgramScene" => {
                params
                    .get("sceneName")
                    .and_then(|v| v.as_str())
                    .map(|name| name == obs_state.current_scene)
                    .unwrap_or(false)
            }
            "ToggleInputMute" => {
                params
                    .get("inputName")
                    .and_then(|v| v.as_str())
                    .and_then(|name| obs_state.muted_inputs.get(name))
                    .copied()
                    .unwrap_or(false)
            }
            _ => false,
        },
        _ => false,
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
    last_down: &mut HashMap<u8, Instant>,
    shutdown: &Arc<AtomicBool>,
    dry_run: bool,
    lcd: Option<&LcdWriter>,
    mute_inputs: &[String],
    button_active: &mut HashMap<u8, bool>,
) -> i32 {
    let has_obs_buttons = config.button.iter().any(|b| matches!(&b.action, Action::Obs { .. }));
    let mut poll_interval = tokio::time::interval(Duration::from_secs(STATE_POLL_INTERVAL_SECS));
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

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
                            // Immediately poll state after action to update LCD faster
                            if has_obs_buttons {
                                if let Some(lcd) = lcd {
                                    update_button_states(config, obs_client, lcd, mute_inputs, button_active).await;
                                }
                            }
                        }
                    }
                    Some(ButtonEvent::Up(btn)) => {
                        tracing::debug!(button = %btn, "Button released");
                    }
                    None => {
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
            _ = poll_interval.tick(), if has_obs_buttons && lcd.is_some() && !dry_run => {
                if let Some(lcd) = lcd {
                    update_button_states(config, obs_client, lcd, mute_inputs, button_active).await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl+C received, shutting down...");
                shutdown.store(true, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(600)).await;
                return 0;
            }
        }
    }
}

/// Poll OBS state and update LCD buttons whose active state has changed.
async fn update_button_states(
    config: &Config,
    obs_client: &mut ObsClient,
    lcd: &LcdWriter,
    mute_inputs: &[String],
    button_active: &mut HashMap<u8, bool>,
) {
    let obs_state = match obs_client.poll_state(mute_inputs).await {
        Some(state) => state,
        None => return,
    };

    for mapping in &config.button {
        if mapping.id < 1 || mapping.id > 9 {
            continue;
        }
        if !matches!(&mapping.action, Action::Obs { .. }) {
            continue;
        }

        let active = is_button_active(mapping, &obs_state);
        let prev = button_active.get(&mapping.id).copied().unwrap_or(false);

        if active != prev {
            tracing::info!(
                button = mapping.id,
                active,
                "Button state changed"
            );
            button_active.insert(mapping.id, active);
            render_button(mapping, lcd, active);
        }
    }
}
