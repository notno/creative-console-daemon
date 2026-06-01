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
/// Cap on the per-tab undo history so a long session can't grow it unbounded.
const UNDO_LIMIT: usize = 50;

struct ConfigTab {
    path: PathBuf,
    log_path: PathBuf,
    model: EditModel,
    current_page: u16,
    selected_id: Option<u8>,
    dirty: bool,
    message: Option<(bool, String)>,
    /// Snapshots taken before each structural edit (create/delete/cut/paste),
    /// newest last. `Ctrl+Z` / Undo pops the latest.
    undo: Vec<EditModel>,
    /// States undone but not yet superseded, newest last. `Ctrl+Y` /
    /// `Ctrl+Shift+Z` / Redo replays them; a fresh structural edit clears it.
    redo: Vec<EditModel>,
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
            undo: Vec::new(),
            redo: Vec::new(),
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
        self.undo.clear();
        self.redo.clear();
        self.message = match err {
            Some(e) => Some((true, e)),
            None => Some((false, "Reloaded from disk.".into())),
        };
    }

    /// Record the current model so the next structural edit can be undone. A
    /// fresh edit invalidates the redo branch.
    fn snapshot(&mut self) {
        self.undo.push(self.model.clone());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Clamp the selection if its cell no longer exists after a history jump.
    fn clamp_selection(&mut self) {
        if let Some(id) = self.selected_id {
            if self.button_index(self.current_page, id).is_none() {
                self.selected_id = None;
            }
        }
    }

    /// Restore the most recent snapshot, if any (and make it redoable).
    fn undo(&mut self) {
        match self.undo.pop() {
            Some(prev) => {
                self.redo.push(std::mem::replace(&mut self.model, prev));
                self.dirty = true;
                self.clamp_selection();
                self.message = Some((false, "Undid last change.".into()));
            }
            None => self.message = Some((false, "Nothing to undo.".into())),
        }
    }

    /// Replay the most recently undone state, if any.
    fn redo(&mut self) {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(&mut self.model, next));
                self.dirty = true;
                self.clamp_selection();
                self.message = Some((false, "Redid last change.".into()));
            }
            None => self.message = Some((false, "Nothing to redo.".into())),
        }
    }

    /// Delete the button on cell `id` of the current page (undoable).
    fn remove_button(&mut self, id: u8) {
        if let Some(i) = self.button_index(self.current_page, id) {
            self.snapshot();
            self.model.buttons.remove(i);
            if self.selected_id == Some(id) {
                self.selected_id = None;
            }
            self.dirty = true;
            self.message = Some((false, format!("Deleted button {id}.")));
        }
    }

    /// Copy the selected button into the shared clipboard.
    fn copy_selected(&mut self, clip: &mut Option<ButtonEdit>) {
        if let Some(id) = self.selected_id {
            if let Some(i) = self.button_index(self.current_page, id) {
                *clip = Some(self.model.buttons[i].clone());
                self.message = Some((false, format!("Copied button {id}.")));
            }
        }
    }

    /// Copy the selected button into the clipboard, then delete it.
    fn cut_selected(&mut self, clip: &mut Option<ButtonEdit>) {
        if let Some(id) = self.selected_id {
            if let Some(i) = self.button_index(self.current_page, id) {
                self.snapshot();
                *clip = Some(self.model.buttons[i].clone());
                self.model.buttons.remove(i);
                self.selected_id = None;
                self.dirty = true;
                self.message = Some((false, format!("Cut button {id}.")));
            }
        }
    }

    /// Paste the clipboard onto the currently selected cell, if any.
    fn paste_selected(&mut self, clip: &Option<ButtonEdit>) {
        if let (Some(src), Some(id)) = (clip.as_ref(), self.selected_id) {
            self.paste_onto(src, id);
        }
    }

    /// Paste a button onto cell `id` of the current page — its id/page are
    /// rewritten to the target, so it works across pages and devices. Replaces
    /// any existing button there.
    fn paste_onto(&mut self, src: &ButtonEdit, id: u8) {
        self.snapshot();
        let mut b = src.clone();
        b.id = id;
        b.page = self.current_page;
        match self.button_index(self.current_page, id) {
            Some(i) => self.model.buttons[i] = b,
            None => self.model.buttons.push(b),
        }
        self.selected_id = Some(id);
        self.dirty = true;
        self.message = Some((false, format!("Pasted into button {id}.")));
    }
}

struct EditorApp {
    tabs: Vec<ConfigTab>,
    active: usize,
    /// Shared across tabs so a button copied from one config (or device) can be
    /// pasted into another. The target cell's id/page is applied on paste.
    clipboard: Option<ButtonEdit>,
}

impl EditorApp {
    fn new(configs: Vec<PathBuf>) -> Self {
        let tabs = configs.into_iter().map(ConfigTab::load).collect();
        Self { tabs, active: 0, clipboard: None }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Refresh the log tail roughly once a second.
        ctx.request_repaint_after(Duration::from_secs(1));

        self.top_panel(ctx);

        // Ctrl+S saves the active tab — works even while a text field has focus.
        let save = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S));

        // Ctrl+C/X/V, undo/redo, and Delete act on the selected button — but not
        // while a text field has focus, so they keep their in-field meaning there
        // (Ctrl+Z then undoes typing via egui's own per-field history).
        let (copy, cut, paste, undo, redo, delete) = if ctx.wants_keyboard_input() {
            (false, false, false, false, false, false)
        } else {
            ctx.input_mut(|i| {
                let copy = i.consume_key(egui::Modifiers::COMMAND, egui::Key::C);
                let cut = i.consume_key(egui::Modifiers::COMMAND, egui::Key::X);
                let paste = i.consume_key(egui::Modifiers::COMMAND, egui::Key::V);
                // Redo (Ctrl+Shift+Z / Ctrl+Y) must be checked before undo:
                // consume_key ignores extra Shift, so plain Ctrl+Z would
                // otherwise also swallow Ctrl+Shift+Z.
                let redo = i
                    .consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z)
                    || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
                let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
                let delete = i.consume_key(egui::Modifiers::NONE, egui::Key::Delete);
                (copy, cut, paste, undo, redo, delete)
            })
        };
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if save && tab.dirty {
                tab.save();
            }
            if copy {
                tab.copy_selected(&mut self.clipboard);
            }
            if cut {
                tab.cut_selected(&mut self.clipboard);
            }
            if paste {
                tab.paste_selected(&self.clipboard);
            }
            if undo {
                tab.undo();
            }
            if redo {
                tab.redo();
            }
            if delete {
                if let Some(id) = tab.selected_id {
                    tab.remove_button(id);
                }
            }
        }

        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.log_panel(ctx);
            tab.left_panel(ctx, &mut self.clipboard);
            tab.button_editor_panel(ctx, &mut self.clipboard);
        }
    }
}

impl EditorApp {
    fn top_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Creative Console");
                ui.separator();

                if let Some(b) = &self.clipboard {
                    ui.label(RichText::new(format!("📋 {}", grid_label(b))).weak())
                        .on_hover_text("Clipboard — Ctrl+V or right-click a cell to paste");
                    ui.separator();
                }

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
                if ui.add_enabled(tab.dirty, save).on_hover_text("Ctrl+S").clicked() {
                    tab.save();
                }
                let undo = egui::Button::new("Undo");
                if ui.add_enabled(!tab.undo.is_empty(), undo).on_hover_text("Ctrl+Z").clicked() {
                    tab.undo();
                }
                let redo = egui::Button::new("Redo");
                if ui.add_enabled(!tab.redo.is_empty(), redo).on_hover_text("Ctrl+Y").clicked() {
                    tab.redo();
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

    fn left_panel(&mut self, ctx: &egui::Context, clip: &mut Option<ButtonEdit>) {
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
                self.button_grid(ui, clip);
                ui.separator();
                ui.collapsing("Device & OBS settings", |ui| {
                    self.settings_ui(ui);
                });
            });
    }

    fn button_grid(&mut self, ui: &mut egui::Ui, clip: &mut Option<ButtonEdit>) {
        let cols: u8 = match self.model.device_type {
            DeviceType::MxCreative => 3,
            DeviceType::StreamdeckXl => 8,
        };
        let max_id = self.model.max_lcd_id();
        let cell = egui::vec2(96.0, 64.0);
        let has_clip = clip.is_some();

        let mut to_select: Option<u8> = None;
        let mut to_create: Option<u8> = None;
        let mut to_copy: Option<u8> = None;
        let mut to_cut: Option<u8> = None;
        let mut to_paste: Option<u8> = None;
        let mut to_delete: Option<u8> = None;

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
                            let resp = ui.add(btn);
                            if resp.clicked() {
                                to_select = Some(id);
                            }
                            resp.context_menu(|ui| {
                                if ui.button("Copy").clicked() {
                                    to_copy = Some(id);
                                    ui.close();
                                }
                                if ui.button("Cut").clicked() {
                                    to_cut = Some(id);
                                    ui.close();
                                }
                                if ui
                                    .add_enabled(has_clip, egui::Button::new("Paste (replace)"))
                                    .clicked()
                                {
                                    to_paste = Some(id);
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Delete").clicked() {
                                    to_delete = Some(id);
                                    ui.close();
                                }
                            });
                        }
                        None => {
                            let btn = egui::Button::new(RichText::new(format!("＋ {id}")).weak())
                                .min_size(cell)
                                .fill(Color32::from_gray(28));
                            let resp = ui.add(btn);
                            if resp.clicked() {
                                to_create = Some(id);
                            }
                            resp.context_menu(|ui| {
                                if ui
                                    .add_enabled(has_clip, egui::Button::new("Paste"))
                                    .clicked()
                                {
                                    to_paste = Some(id);
                                    ui.close();
                                }
                            });
                        }
                    }
                    id += 1;
                }
            });
        }

        if let Some(id) = to_create {
            self.snapshot();
            self.model.buttons.push(ButtonEdit::new(id, self.current_page));
            self.selected_id = Some(id);
            self.dirty = true;
        }
        if let Some(id) = to_select {
            self.selected_id = Some(id);
        }
        if let Some(id) = to_copy {
            self.selected_id = Some(id);
            self.copy_selected(clip);
        }
        if let Some(id) = to_cut {
            self.selected_id = Some(id);
            self.cut_selected(clip);
        }
        if let Some(id) = to_paste {
            if let Some(src) = clip.as_ref() {
                self.paste_onto(src, id);
            }
        }
        if let Some(id) = to_delete {
            self.remove_button(id);
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

    fn button_editor_panel(&mut self, ctx: &egui::Context, clip: &mut Option<ButtonEdit>) {
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

            let has_clip = clip.is_some();
            let mut dirty = false;
            let mut remove = false;
            let mut copy = false;
            let mut cut = false;
            let mut paste = false;
            {
                let b = &mut self.model.buttons[idx];
                ui.horizontal(|ui| {
                    ui.heading(format!("Button {id} (page {})", self.current_page));
                    if ui.button("Copy").clicked() {
                        copy = true;
                    }
                    if ui.button("Cut").clicked() {
                        cut = true;
                    }
                    if ui.add_enabled(has_clip, egui::Button::new("Paste")).clicked() {
                        paste = true;
                    }
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
                self.remove_button(id);
            } else if cut {
                self.cut_selected(clip);
            } else {
                if copy {
                    self.copy_selected(clip);
                }
                if paste {
                    self.paste_selected(clip);
                }
                if dirty {
                    self.dirty = true;
                }
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
