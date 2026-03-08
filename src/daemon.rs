use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::actions::media_keys;
use crate::actions::obs::{ObsClient, ObsState};
use crate::actions::webhook::WebhookClient;
use crate::actions::webhook_poll::WebhookPoller;
use crate::config::{Action, ButtonMapping, Config, DeviceType};
use crate::hid::device;
use crate::hid::lcd::LcdWriter;
use crate::hid::protocol::ButtonEvent;
use crate::hid::streamdeck;

/// Debounce interval: ignore repeated Down events for the same button within this window.
const DEBOUNCE_MS: u64 = 100;

/// How often to poll OBS for state changes (seconds).
const STATE_POLL_INTERVAL_SECS: u64 = 2;

/// Spawn a file watcher for config hot-reload.
/// Returns an mpsc receiver that fires when the config file is modified.
fn spawn_config_watcher(config_path: &Path) -> Option<mpsc::Receiver<()>> {
    let (tx, rx) = mpsc::channel(1);
    let path = config_path.to_path_buf();

    let mut watcher = match notify::recommended_watcher(move |res: Result<notify::Event, _>| {
        if let Ok(event) = res {
            if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                let _ = tx.blocking_send(());
            }
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("Failed to create config file watcher: {e}");
            return None;
        }
    };

    // Watch the parent directory (some editors write to a temp file then rename)
    let watch_path = path.parent().unwrap_or(&path);
    if let Err(e) = watcher.watch(watch_path.as_ref(), RecursiveMode::NonRecursive) {
        tracing::warn!("Failed to watch config directory: {e}");
        return None;
    }

    // Keep the watcher alive by leaking it (it runs until process exit)
    std::mem::forget(watcher);
    tracing::info!("Config hot-reload enabled (watching {:?})", path);
    Some(rx)
}

/// Holds the optional Stream Deck connection for LCD rendering.
enum DeviceHandle {
    MxCreative(Option<LcdWriter>),
    StreamDeck(elgato_streamdeck::AsyncStreamDeck),
}

/// Run the main daemon event loop with optional config path for hot-reload.
pub async fn run(config: Config, shutdown: Arc<AtomicBool>, dry_run: bool, config_path: Option<PathBuf>) -> i32 {
    let device_type = config.device.device_type;

    // Open device and spawn reader based on device type
    let (rx, device_handle) = match device_type {
        DeviceType::MxCreative => {
            let rx = match device::spawn_reader(&config.device, shutdown.clone()) {
                Ok(rx) => rx,
                Err(e) => {
                    tracing::error!("Failed to start HID reader: {}", e);
                    return 1;
                }
            };
            let lcd = open_lcd(&config);
            (rx, DeviceHandle::MxCreative(lcd))
        }
        DeviceType::StreamdeckXl => {
            let (deck, kind) = match streamdeck::connect(config.device.serial.as_deref()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("Failed to connect to Stream Deck: {}", e);
                    return 1;
                }
            };
            tracing::info!("Connected to Stream Deck {:?} ({} keys)", kind, kind.key_count());
            let rx = streamdeck::spawn_reader(deck.clone(), shutdown.clone());
            (rx, DeviceHandle::StreamDeck(deck))
        }
    };

    // Initialize LCD for MX Creative
    if let DeviceHandle::MxCreative(Some(ref lcd)) = device_handle {
        render_page_buttons(&config, lcd, 1, &HashMap::new());
    }

    // Render initial buttons for Stream Deck
    if let DeviceHandle::StreamDeck(ref deck) = device_handle {
        render_streamdeck_buttons(&config, deck, 1, &HashMap::new()).await;
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

    // Page navigation state
    let page_count = config.page_count();
    let mut current_page: u16 = 1;
    tracing::info!(pages = page_count, "Page support initialized");

    // Config hot-reload watcher
    let mut config_rx = config_path.as_deref().and_then(spawn_config_watcher);
    let mut config = config;

    // Webhook poller
    let webhook_poller = if config.webhook_poll.is_empty() {
        None
    } else {
        Some(WebhookPoller::new())
    };

    // Event loop
    run_event_loop(
        rx, &mut config, &mut obs_client, &webhook_client,
        &mut last_down, &shutdown, dry_run,
        &device_handle, &mute_inputs, &mut button_active,
        &mut current_page,
        &mut config_rx, config_path.as_deref(),
        webhook_poller.as_ref(),
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

/// Determine if a button's action is currently "active" based on OBS state.
fn is_button_active(mapping: &ButtonMapping, obs_state: &ObsState) -> bool {
    match &mapping.action {
        Action::Obs { command, params } => match command.as_str() {
            "ToggleRecord" | "StartRecord" | "StopRecord" => obs_state.recording,
            "ToggleRecordPause" | "PauseRecord" | "ResumeRecord" => obs_state.recording_paused,
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

#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    mut rx: mpsc::Receiver<ButtonEvent>,
    config: &mut Config,
    obs_client: &mut ObsClient,
    webhook_client: &WebhookClient,
    last_down: &mut HashMap<u8, Instant>,
    shutdown: &Arc<AtomicBool>,
    dry_run: bool,
    device_handle: &DeviceHandle,
    mute_inputs: &[String],
    button_active: &mut HashMap<u8, bool>,
    current_page: &mut u16,
    config_rx: &mut Option<mpsc::Receiver<()>>,
    config_path: Option<&Path>,
    webhook_poller: Option<&WebhookPoller>,
) -> i32 {
    let lcd = match device_handle {
        DeviceHandle::MxCreative(ref lcd) => lcd.as_ref(),
        DeviceHandle::StreamDeck(_) => None,
    };
    let mut has_obs_buttons = config.button.iter().any(|b| matches!(&b.action, Action::Obs { .. }));
    let mut poll_interval = tokio::time::interval(Duration::from_secs(STATE_POLL_INTERVAL_SECS));
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Webhook poll interval (use shortest configured interval, default 2s)
    let webhook_poll_secs = config.webhook_poll.iter().map(|p| p.interval_secs).min().unwrap_or(2);
    let mut webhook_poll_interval = tokio::time::interval(Duration::from_secs(webhook_poll_secs));
    webhook_poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let has_webhook_polls = webhook_poller.is_some() && !config.webhook_poll.is_empty();

    // Reload debounce: ignore rapid file change events
    let mut last_reload = Instant::now();

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

                        // Handle page navigation (PageLeft=10, PageRight=11)
                        let page_count = config.page_count();
                        if page_count > 1 && (config_id == 10 || config_id == 11) && !config.has_page_button_action(config_id) {
                            let old_page = *current_page;
                            if config_id == 10 {
                                *current_page = if *current_page <= 1 { page_count } else { *current_page - 1 };
                            } else {
                                *current_page = if *current_page >= page_count { 1 } else { *current_page + 1 };
                            }
                            tracing::info!(from = old_page, to = *current_page, "Page changed");
                            match device_handle {
                                DeviceHandle::MxCreative(Some(lcd)) => {
                                    button_active.clear();
                                    render_page_buttons(config, lcd, *current_page, button_active);
                                }
                                DeviceHandle::StreamDeck(deck) => {
                                    button_active.clear();
                                    render_streamdeck_buttons(config, deck, *current_page, button_active).await;
                                }
                                _ => {}
                            }
                            continue;
                        }

                        // Find mapping on the current page
                        let mapping = config.find_button(*current_page, config_id);

                        tracing::info!(button = %btn, config_id, page = *current_page, "Button pressed");

                        if dry_run {
                            tracing::info!(button = %btn, "Dry run: skipping action dispatch");
                        } else if let Some(mapping) = mapping {
                            dispatch_mapping(mapping, obs_client, webhook_client).await;
                            if has_obs_buttons {
                                if let DeviceHandle::MxCreative(Some(lcd)) = device_handle {
                                    update_button_states(config, *current_page, obs_client, lcd, mute_inputs, button_active).await;
                                }
                            }
                        } else {
                            tracing::debug!(config_id, page = *current_page, "No mapping on this page");
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
                    update_button_states(config, *current_page, obs_client, lcd, mute_inputs, button_active).await;
                }
            }
            Some(()) = async {
                match config_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Debounce: editors often trigger multiple events
                if last_reload.elapsed() < Duration::from_millis(500) {
                    continue;
                }
                last_reload = Instant::now();

                if let Some(path) = config_path {
                    // Small delay for editors that write-then-rename
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    match Config::load(path) {
                        Ok(new_config) => {
                            tracing::info!("Config reloaded successfully");
                            *config = new_config;
                            has_obs_buttons = config.button.iter().any(|b| matches!(&b.action, Action::Obs { .. }));

                            // Clamp current page
                            let page_count = config.page_count();
                            if *current_page > page_count {
                                *current_page = 1;
                            }

                            // Update OBS client config
                            *obs_client = ObsClient::new(config.obs.clone());

                            // Re-render LCD
                            match device_handle {
                                DeviceHandle::MxCreative(Some(lcd)) => {
                                    button_active.clear();
                                    render_page_buttons(config, lcd, *current_page, button_active);
                                }
                                DeviceHandle::StreamDeck(deck) => {
                                    button_active.clear();
                                    render_streamdeck_buttons(config, deck, *current_page, button_active).await;
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Config reload failed (keeping current config): {e}");
                        }
                    }
                }
            }
            _ = webhook_poll_interval.tick(), if has_webhook_polls && !dry_run => {
                if let Some(poller) = webhook_poller {
                    for poll_config in &config.webhook_poll {
                        let states = poller.poll(poll_config).await;
                        for (btn_id, active) in &states {
                            let prev = button_active.get(btn_id).copied().unwrap_or(false);
                            if *active != prev {
                                button_active.insert(*btn_id, *active);
                                // Find the button mapping to re-render
                                if let Some(mapping) = config.find_button(*current_page, *btn_id) {
                                    match device_handle {
                                        DeviceHandle::MxCreative(Some(lcd)) => {
                                            render_button(mapping, lcd, *active);
                                        }
                                        DeviceHandle::StreamDeck(deck) => {
                                            render_streamdeck_button_state(mapping, deck, *active).await;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
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

/// Render all LCD buttons for a given page.
fn render_page_buttons(config: &Config, lcd: &LcdWriter, page: u16, active_states: &HashMap<u8, bool>) {
    // Clear buttons that have no mapping on this page
    let page_buttons = config.buttons_on_page(page);
    let mapped_ids: std::collections::HashSet<u8> = page_buttons.iter().map(|b| b.id).collect();
    for id in 1..=9u8 {
        if !mapped_ids.contains(&id) {
            let _ = lcd.clear_button(id);
        }
    }
    // Render mapped buttons
    for mapping in &page_buttons {
        let active = active_states.get(&mapping.id).copied().unwrap_or(false);
        render_button(mapping, lcd, active);
    }
}

/// Dispatch an action from a specific ButtonMapping.
async fn dispatch_mapping(
    mapping: &ButtonMapping,
    obs_client: &mut ObsClient,
    webhook_client: &WebhookClient,
) {
    match &mapping.action {
        Action::Obs { command, params } => {
            if let Err(e) = obs_client.execute(command, params).await {
                tracing::warn!(command, error = %e, "OBS action failed");
            }
        }
        Action::Webhook { method, url, body, headers } => {
            if let Err(e) = webhook_client.send(method, url, body.as_deref(), headers).await {
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

/// Poll OBS state and update LCD buttons whose active state has changed.
async fn update_button_states(
    config: &Config,
    current_page: u16,
    obs_client: &mut ObsClient,
    lcd: &LcdWriter,
    mute_inputs: &[String],
    button_active: &mut HashMap<u8, bool>,
) {
    let obs_state = match obs_client.poll_state(mute_inputs).await {
        Some(state) => state,
        None => return,
    };

    for mapping in config.buttons_on_page(current_page) {
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

// ---------------------------------------------------------------------------
// Stream Deck rendering helpers
// ---------------------------------------------------------------------------

/// Render all buttons for a Stream Deck on a given page.
async fn render_streamdeck_buttons(
    config: &Config,
    deck: &elgato_streamdeck::AsyncStreamDeck,
    page: u16,
    active_states: &HashMap<u8, bool>,
) {
    let kind = deck.kind();
    let key_count = kind.key_count();
    let page_buttons = config.buttons_on_page(page);
    let mapped_ids: std::collections::HashSet<u8> = page_buttons.iter().map(|b| b.id).collect();

    // Clear unmapped buttons
    for id in 1..=key_count {
        if !mapped_ids.contains(&id) {
            if let Err(e) = streamdeck::clear_button(deck, id).await {
                tracing::warn!(button = id, "Failed to clear Stream Deck button: {e}");
            }
        }
    }

    // Render mapped buttons
    for mapping in &page_buttons {
        let active = active_states.get(&mapping.id).copied().unwrap_or(false);
        render_streamdeck_button_state(mapping, deck, active).await;
    }
}

/// Render a single Stream Deck button in its current state.
async fn render_streamdeck_button_state(
    mapping: &ButtonMapping,
    deck: &elgato_streamdeck::AsyncStreamDeck,
    active: bool,
) {
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
        streamdeck::write_button_file(deck, mapping.id, std::path::Path::new(icon_path)).await
    } else if let Some(label_text) = label {
        streamdeck::write_button_label(deck, mapping.id, label_text, fg, bg).await
    } else {
        let auto_label = match &mapping.action {
            Action::Obs { command, .. } => shorten_obs_command(command),
            Action::Media { key } => key.replace('_', " "),
            Action::Webhook { method, .. } => method.clone(),
        };
        streamdeck::write_button_label(deck, mapping.id, &auto_label, fg, bg).await
    };

    if let Err(e) = result {
        tracing::warn!(button = mapping.id, "Failed to render Stream Deck button: {e}");
    }
}
