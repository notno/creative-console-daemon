use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use elgato_streamdeck::{self, AsyncStreamDeck, DeviceStateUpdate};
use elgato_streamdeck::info::Kind;
use image::DynamicImage;

use crate::hid::protocol::ButtonEvent;
use crate::hid::lcd;

/// Open and return an AsyncStreamDeck for the first detected Stream Deck XL.
/// If `serial` is provided, connects to that specific device.
pub fn connect(serial: Option<&str>) -> Result<(AsyncStreamDeck, Kind)> {
    let hidapi = elgato_streamdeck::new_hidapi()
        .context("Failed to initialize HID API for Stream Deck")?;

    let devices = elgato_streamdeck::list_devices(&hidapi);
    if devices.is_empty() {
        anyhow::bail!("No Stream Deck devices found");
    }

    // Log discovered devices
    for (kind, sn) in &devices {
        tracing::info!("Found Stream Deck: {:?} (serial: {})", kind, sn);
    }

    let (kind, sn) = if let Some(target_serial) = serial {
        devices
            .into_iter()
            .find(|(_, sn)| sn == target_serial)
            .with_context(|| format!("Stream Deck with serial '{target_serial}' not found"))?
    } else {
        // Auto-select first XL, or first device if no XL found
        let xl = devices.iter().find(|(k, _)| matches!(k, Kind::Xl | Kind::XlV2 | Kind::XlV2Module));
        if let Some((k, s)) = xl {
            (*k, s.clone())
        } else {
            let (k, s) = devices.into_iter().next().unwrap();
            tracing::warn!("No Stream Deck XL found, using {:?} (serial: {})", k, s);
            (k, s)
        }
    };

    tracing::info!("Connecting to Stream Deck {:?} (serial: {})", kind, sn);
    let deck = AsyncStreamDeck::connect(&hidapi, kind, &sn)
        .map_err(|e| anyhow::anyhow!("Failed to connect to Stream Deck: {e}"))?;

    Ok((deck, kind))
}

/// List all connected Stream Deck devices.
pub fn list_devices() -> Result<()> {
    let hidapi = elgato_streamdeck::new_hidapi()
        .context("Failed to initialize HID API")?;
    let devices = elgato_streamdeck::list_devices(&hidapi);

    if devices.is_empty() {
        println!("No Stream Deck devices found.");
    } else {
        println!("Stream Deck devices:");
        println!("{:-<60}", "");
        for (kind, serial) in &devices {
            println!("  {kind:?}  serial={serial}");
        }
    }
    Ok(())
}

/// Spawn an async reader task that converts Stream Deck button events
/// into our ButtonEvent format and sends them over an mpsc channel.
pub fn spawn_reader(
    deck: AsyncStreamDeck,
    shutdown: Arc<AtomicBool>,
) -> mpsc::Receiver<ButtonEvent> {
    let (tx, rx) = mpsc::channel(64);

    let reader = deck.get_reader();
    tokio::spawn(async move {
        loop {
            if shutdown.load(Ordering::Relaxed) {
                tracing::info!("Stream Deck reader: shutdown");
                break;
            }

            match reader.read(30.0).await {
                Ok(updates) => {
                    for update in updates {
                        let event = match update {
                            // Stream Deck uses 0-indexed keys; our config uses 1-indexed
                            DeviceStateUpdate::ButtonDown(key) => {
                                ButtonEvent::Down(crate::hid::protocol::ButtonId::Lcd(key + 1))
                            }
                            DeviceStateUpdate::ButtonUp(key) => {
                                ButtonEvent::Up(crate::hid::protocol::ButtonId::Lcd(key + 1))
                            }
                            _ => continue,
                        };
                        if tx.send(event).await.is_err() {
                            tracing::info!("Stream Deck reader: channel closed");
                            return;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Stream Deck read error: {e}");
                    break;
                }
            }
        }
    });

    rx
}

/// Write a label image to a Stream Deck button (1-indexed config_id).
pub async fn write_button_label(
    deck: &AsyncStreamDeck,
    config_id: u8,
    text: &str,
    fg: [u8; 3],
    bg: [u8; 3],
    font_scale: Option<u32>,
) -> Result<()> {
    let key = config_id.checked_sub(1)
        .context("Button ID must be >= 1")?;
    let kind = deck.kind();
    let (w, h) = kind.key_image_format().size;
    let img = lcd::label_image_sized(text, fg, bg, w as u32, h as u32, font_scale);
    deck.set_button_image(key, DynamicImage::ImageRgb8(img)).await
        .map_err(|e| anyhow::anyhow!("Failed to set Stream Deck button image: {e}"))?;
    deck.flush().await
        .map_err(|e| anyhow::anyhow!("Failed to flush Stream Deck: {e}"))?;
    Ok(())
}

/// Write an image file to a Stream Deck button (1-indexed config_id).
pub async fn write_button_file(
    deck: &AsyncStreamDeck,
    config_id: u8,
    path: &std::path::Path,
) -> Result<()> {
    let key = config_id.checked_sub(1)
        .context("Button ID must be >= 1")?;
    let img = image::open(path)
        .with_context(|| format!("Failed to load image: {}", path.display()))?;
    deck.set_button_image(key, img).await
        .map_err(|e| anyhow::anyhow!("Failed to set Stream Deck button image: {e}"))?;
    deck.flush().await
        .map_err(|e| anyhow::anyhow!("Failed to flush Stream Deck: {e}"))?;
    Ok(())
}

/// Clear a Stream Deck button (1-indexed config_id).
pub async fn clear_button(deck: &AsyncStreamDeck, config_id: u8) -> Result<()> {
    let key = config_id.checked_sub(1)
        .context("Button ID must be >= 1")?;
    deck.clear_button_image(key).await
        .map_err(|e| anyhow::anyhow!("Failed to clear Stream Deck button: {e}"))?;
    deck.flush().await
        .map_err(|e| anyhow::anyhow!("Failed to flush Stream Deck: {e}"))?;
    Ok(())
}
