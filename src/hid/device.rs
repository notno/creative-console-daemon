use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::config::DeviceConfig;
use crate::hid::protocol::{format_hex, ButtonEvent, ReportParser};

/// HID report buffer size. Must be large enough for the largest report
/// on any interface. The 0x1A10 interface uses extended reports.
const REPORT_BUF_SIZE: usize = 512;

/// Read timeout in milliseconds (allows checking shutdown flag)
const READ_TIMEOUT_MS: i32 = 500;

/// Initialization commands to divert arrow/page buttons.
/// Format: reportId=0x11 (20-byte long), deviceIdx=0xFF, featureIdx=0x0B (REPROG_CONTROLS_V4),
/// function=0x3 (setCidReporting), swId=0xB, CID (2-byte BE), divertFlags=0x03
const INIT_DIVERT_LEFT: [u8; 20] = [
    0x11, 0xFF, 0x0B, 0x3B, 0x01, 0xA1, 0x03, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];
const INIT_DIVERT_RIGHT: [u8; 20] = [
    0x11, 0xFF, 0x0B, 0x3B, 0x01, 0xA2, 0x03, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// Usage ID for the arrow/page button interface (short HID++ reports, reportId 0x11)
const USAGE_ARROW: u16 = 0x1A02;
/// Usage ID for the LCD button interface (reportId 0x13, native — no diversion needed)
const USAGE_LCD: u16 = 0x1A08;

/// List all HID devices matching a given vendor ID.
/// Prints device info including usage_page for debugging.
pub fn list_devices(vid: u16) -> Result<()> {
    let api = hidapi::HidApi::new().context("Failed to initialize HID API")?;

    println!("HID devices for VID 0x{vid:04X}:");
    println!("{:-<90}", "");
    println!(
        "{:<8} {:<8} {:<12} {:<8} {:<6} {:<40}",
        "VID", "PID", "UsagePage", "Usage", "IF#", "Product"
    );
    println!("{:-<90}", "");

    let mut found = false;
    for dev in api.device_list() {
        if dev.vendor_id() == vid {
            found = true;
            println!(
                "0x{:04X}  0x{:04X}  0x{:04X}       0x{:04X}  {:<6} {}",
                dev.vendor_id(),
                dev.product_id(),
                dev.usage_page(),
                dev.usage(),
                dev.interface_number(),
                dev.product_string().unwrap_or("(unknown)")
            );
        }
    }

    if !found {
        println!("No devices found.");
    }

    Ok(())
}

/// Diagnostic mode: try opening each interface for a VID/PID and report capabilities.
pub fn run_diag(vid: u16, pid: u16) -> Result<()> {
    let api = hidapi::HidApi::new().context("Failed to initialize HID API")?;

    println!("=== HID Diagnostic for VID=0x{vid:04X} PID=0x{pid:04X} ===\n");

    let interfaces: Vec<_> = api
        .device_list()
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
        .collect();

    if interfaces.is_empty() {
        println!("No matching devices found.");
        return Ok(());
    }

    for info in &interfaces {
        println!(
            "--- Interface: usage_page=0x{:04X} usage=0x{:04X} if={} ---",
            info.usage_page(),
            info.usage(),
            info.interface_number()
        );
        println!("  Product: {}", info.product_string().unwrap_or("(unknown)"));
        println!("  Path: {:?}", info.path());

        match api.open_path(info.path()) {
            Ok(device) => {
                println!("  Open: OK");

                // Try to get report descriptor
                let mut desc_buf = [0u8; 4096];
                match device.get_report_descriptor(&mut desc_buf) {
                    Ok(len) => {
                        println!("  Report descriptor ({len} bytes): {}", format_hex(&desc_buf[..len.min(64)]));
                        if len > 64 {
                            println!("    ... ({} more bytes)", len - 64);
                        }
                    }
                    Err(e) => println!("  Report descriptor: FAILED ({e})"),
                }

                // Try a quick read
                let mut buf = [0u8; REPORT_BUF_SIZE];
                match device.read_timeout(&mut buf, 1000) {
                    Ok(0) => println!("  Read: timeout (no data in 1s)"),
                    Ok(n) => println!("  Read ({n} bytes): {}", format_hex(&buf[..n])),
                    Err(e) => println!("  Read: FAILED ({e})"),
                }
            }
            Err(e) => {
                println!("  Open: FAILED ({e})");
            }
        }
        println!();
    }

    Ok(())
}

/// Open a specific HID interface by VID/PID/usage_page and usage.
fn open_interface(
    api: &hidapi::HidApi,
    config: &DeviceConfig,
    usage: u16,
    label: &str,
) -> Result<hidapi::HidDevice> {
    let device_info = api
        .device_list()
        .find(|dev| {
            dev.vendor_id() == config.vendor_id
                && dev.product_id() == config.product_id
                && dev.usage_page() == config.usage_page
                && dev.usage() == usage
        });

    let info = match device_info {
        Some(info) => info,
        None => {
            tracing::warn!(
                "No {label} interface found (VID=0x{:04X} PID=0x{:04X} usage_page=0x{:04X} usage=0x{usage:04X})",
                config.vendor_id, config.product_id, config.usage_page
            );
            anyhow::bail!("{label} interface not found (usage=0x{usage:04X})");
        }
    };

    let path = info.path().to_owned();
    let device = api.open_path(&path).with_context(|| {
        format!("Failed to open {label} interface at path: {path:?}")
    })?;

    tracing::info!(
        "Opened {label} interface: {} (usage=0x{usage:04X})",
        info.product_string().unwrap_or("(unknown)")
    );

    Ok(device)
}

/// Open the MX Creative Console Keypad device, filtering by VID/PID/usage_page.
/// Falls back to usage filter if specified in config.
fn open_device(config: &DeviceConfig) -> Result<hidapi::HidDevice> {
    let api = hidapi::HidApi::new().context("Failed to initialize HID API")?;
    let usage = config.usage.unwrap_or(USAGE_ARROW);
    open_interface(&api, config, usage, "primary")
}

/// Send initialization commands to divert arrow/page button events.
/// LCD buttons report natively on the 0x1A08 interface and do NOT need diversion.
fn send_init_commands(device: &hidapi::HidDevice) -> Result<()> {
    device
        .write(&INIT_DIVERT_LEFT)
        .context("Failed to send init command for PageLeft diversion")?;
    tracing::debug!("Sent PageLeft divert command");

    device
        .write(&INIT_DIVERT_RIGHT)
        .context("Failed to send init command for PageRight diversion")?;
    tracing::debug!("Sent PageRight divert command");

    tracing::info!("Arrow button diversions enabled");
    Ok(())
}

/// Run the HID read loop in raw dump mode.
/// Prints each report as hex to stdout without parsing.
pub fn run_raw_dump(config: &DeviceConfig, shutdown: Arc<AtomicBool>) -> Result<()> {
    let device = open_device(config)?;
    if let Err(e) = send_init_commands(&device) {
        tracing::warn!("Init commands failed (may not be needed on this interface): {}", e);
    }

    println!("Raw dump mode. Press Ctrl+C to stop.\n");

    let mut buf = [0u8; REPORT_BUF_SIZE];
    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("Shutdown signal received, stopping raw dump");
            break;
        }

        match device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => continue, // Timeout, no data
            Ok(n) => {
                let now = chrono::Local::now().format("%H:%M:%S%.3f");
                println!(
                    "[{}] reportId=0x{:02X} len={}: {}",
                    now,
                    buf[0],
                    n,
                    format_hex(&buf[..n])
                );
            }
            Err(e) => {
                tracing::error!("HID read error: {}", e);
                return Err(anyhow::anyhow!("Device read error: {e}"));
            }
        }
    }

    Ok(())
}

/// Spawn a read loop on a single HID device handle.
/// Sends parsed button events to `tx`. Stops on shutdown or device error.
fn reader_loop(
    device: hidapi::HidDevice,
    tx: mpsc::Sender<ButtonEvent>,
    shutdown: Arc<AtomicBool>,
    label: &'static str,
) {
    let mut parser = ReportParser::new();
    let mut buf = [0u8; REPORT_BUF_SIZE];

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("{label} reader: shutdown signal received");
            break;
        }

        match device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => continue,
            Ok(n) => {
                let events = parser.parse(&buf[..n]);
                for event in events {
                    if tx.blocking_send(event).is_err() {
                        tracing::info!("{label} reader: channel closed, stopping");
                        return;
                    }
                }
            }
            Err(e) => {
                tracing::error!("{label} reader: HID read error (device disconnected?): {e}");
                break;
            }
        }
    }
}

/// Spawn dual HID reader threads (arrow + LCD interfaces).
/// Returns an mpsc receiver for button events from both interfaces.
/// Falls back to single-interface if LCD interface can't be opened.
pub fn spawn_reader(
    config: &DeviceConfig,
    shutdown: Arc<AtomicBool>,
) -> Result<mpsc::Receiver<ButtonEvent>> {
    let api = hidapi::HidApi::new().context("Failed to initialize HID API")?;
    let (tx, rx) = mpsc::channel(64);

    // Open arrow interface (0x1A02) — required
    let arrow_device = open_interface(&api, config, USAGE_ARROW, "arrow")?;
    send_init_commands(&arrow_device)?;

    // Open LCD interface (0x1A08) — optional (graceful degradation)
    let lcd_device = match open_interface(&api, config, USAGE_LCD, "LCD") {
        Ok(dev) => {
            tracing::info!("LCD interface opened — LCD button events will be captured");
            Some(dev)
        }
        Err(e) => {
            tracing::warn!("LCD interface not available: {e}. Only arrow buttons will work.");
            None
        }
    };

    // Spawn arrow reader thread
    let tx_arrow = tx.clone();
    let shutdown_arrow = shutdown.clone();
    std::thread::Builder::new()
        .name("hid-arrow".into())
        .spawn(move || reader_loop(arrow_device, tx_arrow, shutdown_arrow, "arrow"))
        .context("Failed to spawn arrow reader thread")?;

    // Spawn LCD reader thread (if available)
    if let Some(lcd_dev) = lcd_device {
        let tx_lcd = tx;
        let shutdown_lcd = shutdown;
        std::thread::Builder::new()
            .name("hid-lcd".into())
            .spawn(move || reader_loop(lcd_dev, tx_lcd, shutdown_lcd, "LCD"))
            .context("Failed to spawn LCD reader thread")?;
    }

    Ok(rx)
}
