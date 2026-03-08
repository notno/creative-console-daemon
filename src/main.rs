mod actions;
mod config;
mod daemon;
mod hid;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "creative-console-daemon")]
#[command(about = "HID daemon for the Logitech MX Creative Console")]
struct Cli {
    /// Path to the TOML config file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// List all HID devices for the configured vendor ID
    #[arg(long)]
    list_devices: bool,

    /// Raw dump mode: print HID reports as hex without parsing
    #[arg(long)]
    raw_dump: bool,

    /// Diagnostic mode: try opening each HID interface and report capabilities
    #[arg(long)]
    diag: bool,

    /// Dry run: connect and log button events, but don't dispatch actions
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // --diag: diagnostic mode
    if cli.diag {
        let (vid, pid) = if cli.config.exists() {
            match config::Config::load(&cli.config) {
                Ok(c) => (c.device.vendor_id, c.device.product_id),
                Err(_) => (0x046D, 0xC354),
            }
        } else {
            (0x046D, 0xC354)
        };

        if let Err(e) = hid::device::run_diag(vid, pid) {
            tracing::error!("Diagnostic error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    // --list-devices: enumerate and exit
    if cli.list_devices {
        let vid = if cli.config.exists() {
            match config::Config::load(&cli.config) {
                Ok(c) => c.device.vendor_id,
                Err(_) => 0x046D, // Default Logitech VID
            }
        } else {
            0x046D
        };

        if let Err(e) = hid::device::list_devices(vid) {
            tracing::error!("Failed to list MX Creative devices: {}", e);
        }
        // Also list Stream Deck devices
        if let Err(e) = hid::streamdeck::list_devices() {
            tracing::error!("Failed to list Stream Deck devices: {}", e);
        }
        return;
    }

    // Load config
    let config = match config::Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Configuration error: {:#}", e);
            std::process::exit(1);
        }
    };

    let shutdown = Arc::new(AtomicBool::new(false));

    // --raw-dump: hex dump mode
    if cli.raw_dump {
        if let Err(e) = hid::device::run_raw_dump(&config.device, shutdown) {
            tracing::error!("Raw dump error: {}", e);
            std::process::exit(2);
        }
        return;
    }

    // Run daemon
    tracing::info!("Starting Creative Console daemon");
    let config_path = std::fs::canonicalize(&cli.config).unwrap_or_else(|_| cli.config.clone());
    let exit_code = daemon::run(config, shutdown, cli.dry_run, Some(config_path)).await;

    match exit_code {
        0 => tracing::info!("Clean shutdown"),
        2 => tracing::error!("Exiting with code 2 (device disconnected). Use restart.ps1 for auto-restart."),
        code => tracing::error!("Exiting with code {}", code),
    }

    std::process::exit(exit_code);
}
