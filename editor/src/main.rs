#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Two run modes in one binary:
//!
//! * default (startup / launch.ps1): a resident **tray + supervisor** with no
//!   GUI window. A Win32 message pump keeps the tray responsive and the daemon
//!   watchdog(s) ticking. Supervises one daemon process per config passed on
//!   the command line (e.g. an MX config + a Stream Deck config, so both
//!   devices run at once). "Open editor" spawns the editor as a child process.
//! * `--edit`: a **visible** eframe editor window. Edits any of the configs it
//!   was given (one tab per config); closing it exits this child process.
//!
//! Keeping the always-on part windowless avoids eframe's hidden-window trap.

mod model;
mod supervisor;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use creative_console_daemon::config::{Config, DeviceType};
use eframe::egui;
use egui::{Color32, RichText};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

use model::{ActionKind, ButtonEdit, EditModel, HTTP_METHODS, MEDIA_KEYS, SHELL_OUTPUTS};
use supervisor::Supervisor;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let edit_mode = args.iter().any(|a| a == "--edit");
    let mut configs: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(|a| {
            let p = PathBuf::from(a);
            std::fs::canonicalize(&p).unwrap_or(p)
        })
        .collect();
    if configs.is_empty() {
        configs.push(PathBuf::from("config.toml"));
    }

    if edit_mode {
        run_editor(configs)
    } else {
        run_tray(configs);
        Ok(())
    }
}

/// The daemon binary sits next to this binary in the shared target dir.
fn daemon_exe_path() -> PathBuf {
    let mut p = std::env::current_exe().unwrap_or_default();
    p.pop();
    p.push("creative-console-daemon.exe");
    p
}

/// Per-config log file, e.g. `config.mx.toml` → `config.mx.toml.log`, so two
/// daemons don't share one log.
fn log_path_for(config: &Path) -> PathBuf {
    let name = config.file_name().and_then(|s| s.to_str()).unwrap_or("daemon");
    config.parent().unwrap_or_else(|| Path::new(".")).join(format!("{name}.log"))
}

fn config_display_name(config: &Path) -> String {
    config.file_name().and_then(|s| s.to_str()).unwrap_or("config").to_string()
}

/// Build a 32×32 RGBA tray icon (teal square with a darker border).
fn tray_icon_image() -> Icon {
    const N: u32 = 32;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let border = x < 2 || y < 2 || x >= N - 2 || y >= N - 2;
            let (r, g, b) = if border { (20, 40, 45) } else { (30, 160, 170) };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Icon::from_rgba(rgba, N, N).expect("valid icon dimensions")
}

// ===========================================================================
// Resident tray + supervisor(s)
// ===========================================================================

fn run_tray(configs: Vec<PathBuf>) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };

    let mut sups: Vec<Supervisor> = configs
        .iter()
        .map(|c| {
            let mut s = Supervisor::new(daemon_exe_path(), c.clone(), &log_path_for(c));
            s.start();
            s
        })
        .collect();

    let menu = Menu::new();
    let open = MenuItem::new("Open editor", true, None);
    let open_log = MenuItem::new("Open logs", true, None);
    let start = MenuItem::new("Start daemons", true, None);
    let stop = MenuItem::new("Stop daemons", true, None);
    let restart = MenuItem::new("Restart daemons", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let _ = menu.append(&open);
    let _ = menu.append(&open_log);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&start);
    let _ = menu.append(&stop);
    let _ = menu.append(&restart);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit);
    let (id_open, id_open_log, id_start, id_stop, id_restart, id_quit) = (
        open.id().clone(),
        open_log.id().clone(),
        start.id().clone(),
        stop.id().clone(),
        restart.id().clone(),
        quit.id().clone(),
    );

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Creative Console")
        .with_icon(tray_icon_image())
        .build()
        .expect("failed to create tray icon");

    let mut editor_child: Option<Child> = None;
    let mut last_summary = String::new();

    'main: loop {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == id_open {
                open_editor(&mut editor_child, &configs);
            } else if event.id == id_open_log {
                for c in &configs {
                    let _ = Command::new("notepad.exe").arg(log_path_for(c)).spawn();
                }
            } else if event.id == id_start {
                sups.iter_mut().for_each(Supervisor::start);
            } else if event.id == id_stop {
                sups.iter_mut().for_each(Supervisor::stop);
            } else if event.id == id_restart {
                sups.iter_mut().for_each(Supervisor::restart);
            } else if event.id == id_quit {
                sups.iter_mut().for_each(Supervisor::stop);
                if let Some(c) = editor_child.as_mut() {
                    let _ = c.kill();
                }
                break 'main;
            }
        }

        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::DoubleClick { .. } = event {
                open_editor(&mut editor_child, &configs);
            }
        }

        sups.iter_mut().for_each(Supervisor::poll);

        let summary = configs
            .iter()
            .zip(&sups)
            .map(|(c, s)| format!("{}: {}", config_display_name(c), s.status.label()))
            .collect::<Vec<_>>()
            .join("  |  ");
        if summary != last_summary {
            last_summary = summary.clone();
            let _ = tray.set_tooltip(Some(format!("Creative Console — {summary}")));
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Launch the editor window as a child process (deduplicated: no-op if one is
/// already open), passing all configs so it can edit any of them.
fn open_editor(child: &mut Option<Child>, configs: &[PathBuf]) {
    if child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None))) {
        return;
    }
    let exe = std::env::current_exe().unwrap_or_default();
    *child = Command::new(exe).arg("--edit").args(configs).spawn().ok();
}

// ===========================================================================
// Editor window (child process)
// ===========================================================================

fn run_editor(configs: Vec<PathBuf>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Creative Console — Editor")
            .with_inner_size([1040.0, 680.0])
            .with_min_inner_size([720.0, 460.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Creative Console Editor",
        options,
        Box::new(move |_cc| Ok(Box::new(EditorApp::new(configs)))),
    )
}

fn load_model(path: &Path) -> (EditModel, Option<String>) {
    match Config::load(path) {
        Ok(c) => (EditModel::from_config(&c), None),
        Err(e) => (empty_model(), Some(format!("Could not load config: {e:#}"))),
    }
}

fn empty_model() -> EditModel {
    use model::ObsEdit;
    EditModel {
        device_type: DeviceType::MxCreative,
        vendor_id: 0x046D,
        product_id: 0xC354,
        usage_page: 0xFF43,
        usage: None,
        serial: String::new(),
        obs_enabled: false,
        obs: ObsEdit { host: "localhost".into(), port: 4455, password: String::new() },
        buttons: Vec::new(),
        webhook_poll: Vec::new(),
    }
}

fn read_log_tail(path: &Path, max: usize) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let lines: Vec<String> = s.lines().map(str::to_string).collect();
            let start = lines.len().saturating_sub(max);
            lines[start..].to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// One open config the editor can edit. Each keeps its own edits/selection so
/// switching tabs doesn't lose unsaved work.
struct ConfigTab {
    path: PathBuf,
    log_path: PathBuf,
    model: EditModel,
    current_page: u16,
    selected_id: Option<u8>,
    dirty: bool,
    message: Option<(bool, String)>,
}

impl ConfigTab {
    fn load(path: PathBuf) -> Self {
        let (model, err) = load_model(&path);
        let log_path = log_path_for(&path);
        Self {
            path,
            log_path,
            model,
            current_page: 1,
            selected_id: None,
            dirty: false,
            message: err.map(|e| (true, e)),
        }
    }

    fn button_index(&self, page: u16, id: u8) -> Option<usize> {
        self.model.buttons.iter().position(|b| b.page == page && b.id == id)
    }

    fn save(&mut self) {
        let toml = self.model.to_toml();
        if let Err(e) = Config::parse(&toml) {
            self.message = Some((true, format!("Not saved — invalid config: {e:#}")));
            return;
        }
        match std::fs::write(&self.path, &toml) {
            Ok(()) => {
                self.dirty = false;
                self.message = Some((false, "Saved. Daemon will hot-reload.".into()));
            }
            Err(e) => self.message = Some((true, format!("Write failed: {e}"))),
        }
    }

    fn reload(&mut self) {
        let (model, err) = load_model(&self.path);
        self.model = model;
        self.current_page = 1;
        self.selected_id = None;
        self.dirty = false;
        self.message = match err {
            Some(e) => Some((true, e)),
            None => Some((false, "Reloaded from disk.".into())),
        };
    }
}

struct EditorApp {
    tabs: Vec<ConfigTab>,
    active: usize,
}

impl EditorApp {
    fn new(configs: Vec<PathBuf>) -> Self {
        let tabs = configs.into_iter().map(ConfigTab::load).collect();
        Self { tabs, active: 0 }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Refresh the log tail roughly once a second.
        ctx.request_repaint_after(Duration::from_secs(1));

        self.top_panel(ctx);

        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.log_panel(ctx);
            tab.left_panel(ctx);
            tab.button_editor_panel(ctx);
        }
    }
}

impl EditorApp {
    fn top_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Creative Console");
                ui.separator();

                // Config selector (one tab per open config).
                if self.tabs.len() > 1 {
                    let current = config_display_name(&self.tabs[self.active].path);
                    egui::ComboBox::from_id_salt("config_select")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (i, tab) in self.tabs.iter().enumerate() {
                                let name = config_display_name(&tab.path);
                                let label = if tab.dirty { format!("{name} *") } else { name };
                                ui.selectable_value(&mut self.active, i, label);
                            }
                        });
                    ui.separator();
                }

                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                let save = egui::Button::new(if tab.dirty { "Save *" } else { "Save" });
                if ui.add_enabled(tab.dirty, save).clicked() {
                    tab.save();
                }
                if ui.button("Reload from disk").clicked() {
                    tab.reload();
                }
                ui.separator();
                ui.label(RichText::new("Daemons are controlled from the tray icon.").weak());
            });

            if let Some(tab) = self.tabs.get(self.active) {
                ui.label(RichText::new(tab.path.display().to_string()).weak());
                if let Some((is_err, text)) = &tab.message {
                    let color = if *is_err { Color32::LIGHT_RED } else { Color32::LIGHT_GREEN };
                    ui.colored_label(color, text);
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Per-config UI panels
// ---------------------------------------------------------------------------

impl ConfigTab {
    fn log_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("log")
            .resizable(true)
            .default_height(120.0)
            .show(ctx, |ui| {
                ui.label(RichText::new(format!("Daemon log — {}", config_display_name(&self.path))).strong());
                let lines = read_log_tail(&self.log_path, 800);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if lines.is_empty() {
                            ui.weak("(no log yet)");
                        }
                        for line in &lines {
                            ui.monospace(line);
                        }
                    });
            });
    }

    fn left_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .resizable(true)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Page:");
                    let pages = self.model.page_count().max(self.current_page);
                    for p in 1..=pages {
                        if ui.selectable_label(self.current_page == p, p.to_string()).clicked() {
                            self.current_page = p;
                            self.selected_id = None;
                        }
                    }
                    if ui.button("+ page").clicked() {
                        self.current_page = pages + 1;
                        self.selected_id = None;
                    }
                });
                ui.separator();
                self.button_grid(ui);
                ui.separator();
                ui.collapsing("Device & OBS settings", |ui| {
                    self.settings_ui(ui);
                });
            });
    }

    fn button_grid(&mut self, ui: &mut egui::Ui) {
        let cols: u8 = match self.model.device_type {
            DeviceType::MxCreative => 3,
            DeviceType::StreamdeckXl => 8,
        };
        let max_id = self.model.max_lcd_id();
        let cell = egui::vec2(96.0, 64.0);

        let mut to_select: Option<u8> = None;
        let mut to_create: Option<u8> = None;

        let mut id: u8 = 1;
        while id <= max_id {
            ui.horizontal(|ui| {
                for _ in 0..cols {
                    if id > max_id {
                        break;
                    }
                    let idx = self.button_index(self.current_page, id);
                    let selected = self.selected_id == Some(id);
                    match idx {
                        Some(i) => {
                            let b = &self.model.buttons[i];
                            let text = grid_label(b);
                            let fg = Color32::from_rgb(b.fg[0], b.fg[1], b.fg[2]);
                            let bg = Color32::from_rgb(b.bg[0], b.bg[1], b.bg[2]);
                            let mut btn = egui::Button::new(RichText::new(text).color(fg))
                                .min_size(cell)
                                .fill(bg);
                            if selected {
                                btn = btn.stroke(egui::Stroke::new(2.0, Color32::YELLOW));
                            }
                            if ui.add(btn).clicked() {
                                to_select = Some(id);
                            }
                        }
                        None => {
                            let btn = egui::Button::new(RichText::new(format!("＋ {id}")).weak())
                                .min_size(cell)
                                .fill(Color32::from_gray(28));
                            if ui.add(btn).clicked() {
                                to_create = Some(id);
                            }
                        }
                    }
                    id += 1;
                }
            });
        }

        if let Some(id) = to_create {
            self.model.buttons.push(ButtonEdit::new(id, self.current_page));
            self.selected_id = Some(id);
            self.dirty = true;
        }
        if let Some(id) = to_select {
            self.selected_id = Some(id);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_label("Device")
            .selected_text(match self.model.device_type {
                DeviceType::MxCreative => "MX Creative",
                DeviceType::StreamdeckXl => "Stream Deck XL",
            })
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(self.model.device_type == DeviceType::MxCreative, "MX Creative")
                    .clicked()
                {
                    self.model.device_type = DeviceType::MxCreative;
                    self.dirty = true;
                }
                if ui
                    .selectable_label(
                        self.model.device_type == DeviceType::StreamdeckXl,
                        "Stream Deck XL",
                    )
                    .clicked()
                {
                    self.model.device_type = DeviceType::StreamdeckXl;
                    self.dirty = true;
                }
            });

        if self.model.device_type == DeviceType::StreamdeckXl {
            ui.horizontal(|ui| {
                ui.label("Serial:");
                if ui.text_edit_singleline(&mut self.model.serial).changed() {
                    self.dirty = true;
                }
            });
        }

        ui.separator();
        if ui.checkbox(&mut self.model.obs_enabled, "OBS WebSocket").changed() {
            self.dirty = true;
        }
        if self.model.obs_enabled {
            ui.horizontal(|ui| {
                ui.label("Host:");
                if ui.text_edit_singleline(&mut self.model.obs.host).changed() {
                    self.dirty = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Port:");
                if ui.add(egui::DragValue::new(&mut self.model.obs.port)).changed() {
                    self.dirty = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Password:");
                if ui
                    .add(egui::TextEdit::singleline(&mut self.model.obs.password).password(true))
                    .changed()
                {
                    self.dirty = true;
                }
            });
        }
    }

    fn button_editor_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(id) = self.selected_id else {
                ui.centered_and_justified(|ui| {
                    ui.label("Select a button on the left to edit it.");
                });
                return;
            };
            let Some(idx) = self.button_index(self.current_page, id) else {
                self.selected_id = None;
                return;
            };

            let mut dirty = false;
            let mut remove = false;
            {
                let b = &mut self.model.buttons[idx];
                ui.horizontal(|ui| {
                    ui.heading(format!("Button {id} (page {})", self.current_page));
                    if ui.button("Remove").clicked() {
                        remove = true;
                    }
                });
                ui.separator();

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    dirty |= action_ui(ui, b);
                    ui.separator();
                    dirty |= appearance_ui(ui, b);
                });
            }

            if remove {
                self.model.buttons.remove(idx);
                self.selected_id = None;
                self.dirty = true;
            } else if dirty {
                self.dirty = true;
            }
        });
    }
}

/// Label shown on a grid cell (mirrors the daemon's auto-label fallback).
fn grid_label(b: &ButtonEdit) -> String {
    if !b.label.is_empty() {
        return b.label.clone();
    }
    match b.kind {
        ActionKind::Obs => {
            if b.obs_command.is_empty() {
                "OBS".into()
            } else {
                b.obs_command.clone()
            }
        }
        ActionKind::Webhook => b.wh_method.clone(),
        ActionKind::Media => b.media_key.replace('_', " "),
        ActionKind::Hotkey => b.hotkey_keys.join("+"),
        ActionKind::Shell => b.shell_cmd.clone(),
    }
}

// ---------------------------------------------------------------------------
// Per-button form helpers (borrow only the ButtonEdit)
// ---------------------------------------------------------------------------

fn action_ui(ui: &mut egui::Ui, b: &mut ButtonEdit) -> bool {
    let mut dirty = false;

    egui::ComboBox::from_label("Action")
        .selected_text(b.kind.label())
        .show_ui(ui, |ui| {
            for k in ActionKind::ALL {
                if ui.selectable_label(b.kind == *k, k.label()).clicked() {
                    b.kind = *k;
                    dirty = true;
                }
            }
        });

    ui.add_space(4.0);

    match b.kind {
        ActionKind::Obs => {
            dirty |= labeled_text(ui, "Command", &mut b.obs_command);
            ui.label("Params:");
            dirty |= pair_list(ui, &mut b.obs_params, "key", "value");
        }
        ActionKind::Webhook => {
            dirty |= combo_str(ui, "Method", &mut b.wh_method, HTTP_METHODS);
            dirty |= labeled_text(ui, "URL", &mut b.wh_url);
            dirty |= labeled_text(ui, "Body", &mut b.wh_body);
            dirty |= labeled_text(ui, "Release URL", &mut b.wh_release_url);
            ui.label("Headers:");
            dirty |= pair_list(ui, &mut b.wh_headers, "header", "value");
        }
        ActionKind::Media => {
            dirty |= combo_str(ui, "Key", &mut b.media_key, MEDIA_KEYS);
        }
        ActionKind::Hotkey => {
            ui.label("Keys (pressed together, released in reverse):");
            dirty |= string_list(ui, &mut b.hotkey_keys, "key");
            if ui.checkbox(&mut b.hotkey_hold, "Hold while pressed (push-to-talk)").changed() {
                dirty = true;
            }
        }
        ActionKind::Shell => {
            dirty |= labeled_text(ui, "Command", &mut b.shell_cmd);
            ui.label("Arguments:");
            dirty |= string_list(ui, &mut b.shell_args, "arg");
            dirty |= combo_str(ui, "Output", &mut b.shell_output, SHELL_OUTPUTS);
            if ui.checkbox(&mut b.shell_trim, "Trim stdout").changed() {
                dirty = true;
            }
        }
    }
    dirty
}

fn appearance_ui(ui: &mut egui::Ui, b: &mut ButtonEdit) -> bool {
    let mut dirty = false;
    ui.label(RichText::new("Appearance").strong());
    dirty |= labeled_multiline(ui, "Label (Enter = new line)", &mut b.label);
    dirty |= labeled_text(ui, "Icon path", &mut b.icon);
    ui.horizontal(|ui| {
        ui.label("Text / background:");
        if ui.color_edit_button_srgb(&mut b.fg).changed() {
            dirty = true;
        }
        if ui.color_edit_button_srgb(&mut b.bg).changed() {
            dirty = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label("Font scale:");
        if ui.add(egui::DragValue::new(&mut b.font_scale).range(1..=6)).changed() {
            dirty = true;
        }
    });

    if ui.checkbox(&mut b.has_active, "Distinct active state").changed() {
        dirty = true;
    }
    if b.has_active {
        ui.indent("active", |ui| {
            dirty |= labeled_multiline(ui, "Active label (Enter = new line)", &mut b.active_label);
            dirty |= labeled_text(ui, "Active icon path", &mut b.active_icon);
            ui.horizontal(|ui| {
                ui.label("Active text / background:");
                if ui.color_edit_button_srgb(&mut b.active_fg).changed() {
                    dirty = true;
                }
                if ui.color_edit_button_srgb(&mut b.active_bg).changed() {
                    dirty = true;
                }
            });
        });
    }
    dirty
}

fn labeled_text(ui: &mut egui::Ui, label: &str, value: &mut String) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        if ui.text_edit_singleline(value).changed() {
            changed = true;
        }
    });
    changed
}

/// Multi-line text field — for labels, which may contain `\n` line breaks that
/// the daemon renders as separate lines on the LCD. A single-line field would
/// draw embedded newlines as tofu squares.
fn labeled_multiline(ui: &mut egui::Ui, label: &str, value: &mut String) -> bool {
    ui.label(label);
    ui.add(
        egui::TextEdit::multiline(value)
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    )
    .changed()
}

fn combo_str(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str]) -> bool {
    let mut changed = false;
    egui::ComboBox::from_label(label)
        .selected_text(value.clone())
        .show_ui(ui, |ui| {
            for opt in options {
                if ui.selectable_label(value == *opt, *opt).clicked() {
                    *value = (*opt).to_string();
                    changed = true;
                }
            }
        });
    changed
}

fn string_list(ui: &mut egui::Ui, items: &mut Vec<String>, noun: &str) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    for (i, item) in items.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if ui.text_edit_singleline(item).changed() {
                changed = true;
            }
            if ui.small_button("✕").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        items.remove(i);
        changed = true;
    }
    if ui.button(format!("+ {noun}")).clicked() {
        items.push(String::new());
        changed = true;
    }
    changed
}

fn pair_list(ui: &mut egui::Ui, items: &mut Vec<(String, String)>, key_hint: &str, val_hint: &str) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    for (i, (k, v)) in items.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if ui
                .add(egui::TextEdit::singleline(k).hint_text(key_hint).desired_width(120.0))
                .changed()
            {
                changed = true;
            }
            if ui
                .add(egui::TextEdit::singleline(v).hint_text(val_hint).desired_width(200.0))
                .changed()
            {
                changed = true;
            }
            if ui.small_button("✕").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        items.remove(i);
        changed = true;
    }
    if ui.button("+ pair").clicked() {
        items.push((String::new(), String::new()));
        changed = true;
    }
    changed
}
