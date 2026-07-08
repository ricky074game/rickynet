//! The egui GUI (PdaNet-style): a big Connect/Disconnect toggle, an explicit
//! state indicator, transport selector, live byte counters, a status log, and
//! minimize-to-tray. All heavy work runs on the worker thread; this only reads
//! shared atomic state.

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, RichText, Sense};
use rickynet_wire::DEFAULT_PORT;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::args::Args;
use crate::icon;
use crate::state::{ConnState, Shared};
use crate::transport::TransportKind;
use crate::worker;

const GREEN: Color32 = Color32::from_rgb(46, 204, 113);
const AMBER: Color32 = Color32::from_rgb(243, 156, 18);
const RED: Color32 = Color32::from_rgb(231, 76, 60);
const GRAY: Color32 = Color32::from_rgb(149, 165, 166);

enum TrayAction {
    ToggleWindow,
    Show,
    ConnectDisconnect,
    Quit,
}

pub fn run(args: Args, elevated: bool) {
    let shared = Shared::new();
    shared.set_admin(elevated);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([360.0, 440.0])
            .with_min_inner_size([340.0, 400.0])
            .with_icon(icon::egui_icon())
            .with_title("RickyNet"),
        ..Default::default()
    };

    let result = eframe::run_native(
        "RickyNet",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, shared, args)))),
    );
    if let Err(e) = result {
        eprintln!("RickyNet GUI failed: {e}");
    }
}

struct App {
    shared: Arc<Shared>,
    args: Args,
    transport: TransportKind,
    phone_ip: String,
    port_str: String,
    worker: Option<std::thread::JoinHandle<()>>,
    logo: egui::TextureHandle,
    _tray: Option<TrayIcon>,
    actions: Arc<Mutex<VecDeque<TrayAction>>>,
    quit: bool,
    hidden: bool,
    last_sample: Instant,
    last_rx: u64,
    last_tx: u64,
    rate_rx: f64,
    rate_tx: f64,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, shared: Arc<Shared>, args: Args) -> Self {
        let (rgba, size) = icon::logo_rgba();
        let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
        let logo = cc
            .egui_ctx
            .load_texture("rickynet-logo", image, egui::TextureOptions::LINEAR);

        let transport = args.transport;
        let phone_ip = args.phone_ip.clone().unwrap_or_default();
        let port_str = args.port.to_string();

        let actions = Arc::new(Mutex::new(VecDeque::new()));
        let tray = build_tray(cc.egui_ctx.clone(), actions.clone());
        if tray.is_none() {
            log::warn!("system tray unavailable; closing the window will quit");
        }

        App {
            shared,
            args,
            transport,
            phone_ip,
            port_str,
            worker: None,
            logo,
            _tray: tray,
            actions,
            quit: false,
            hidden: false,
            last_sample: Instant::now(),
            last_rx: 0,
            last_tx: 0,
            rate_rx: 0.0,
            rate_tx: 0.0,
        }
    }

    fn busy(&self) -> bool {
        matches!(
            self.shared.state(),
            ConnState::Connecting | ConnState::Connected
        )
    }

    fn toggle_connection(&mut self) {
        if self.busy() {
            self.shared.request_stop();
        } else {
            self.start_worker();
        }
    }

    fn start_worker(&mut self) {
        if !self.shared.is_admin() {
            self.shared.set_error(
                "needs Administrator to create the network adapter and set routes",
            );
            return;
        }
        let mut args = self.args.clone();
        args.transport = self.transport;
        args.port = self.port_str.parse().unwrap_or(DEFAULT_PORT);
        if self.transport == TransportKind::Wifi {
            if self.phone_ip.parse::<Ipv4Addr>().is_err() {
                self.shared
                    .set_error(format!("invalid phone IP '{}'", self.phone_ip));
                return;
            }
            args.phone_ip = Some(self.phone_ip.clone());
        } else {
            args.phone_ip = None;
        }
        let shared = self.shared.clone();
        self.worker = Some(std::thread::spawn(move || worker::run_connect(shared, args)));
    }

    fn handle_tray_actions(&mut self, ctx: &egui::Context) {
        let drained: Vec<TrayAction> = {
            let mut q = self.actions.lock().unwrap();
            q.drain(..).collect()
        };
        for a in drained {
            match a {
                TrayAction::ToggleWindow => {
                    if self.hidden {
                        self.show_window(ctx);
                    } else {
                        self.hide_window(ctx);
                    }
                }
                TrayAction::Show => self.show_window(ctx),
                TrayAction::ConnectDisconnect => self.toggle_connection(),
                TrayAction::Quit => {
                    self.quit = true;
                    self.shared.request_stop();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn sample_rates(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_sample).as_secs_f64();
        if dt >= 0.5 {
            let rx = self.shared.rx();
            let tx = self.shared.tx();
            self.rate_rx = rx.saturating_sub(self.last_rx) as f64 / dt;
            self.rate_tx = tx.saturating_sub(self.last_tx) as f64 / dt;
            self.last_rx = rx;
            self.last_tx = tx;
            self.last_sample = now;
        }
    }

    fn reap_worker(&mut self) {
        let finished = self
            .worker
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(false);
        if finished {
            self.worker.take();
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_tray_actions(ctx);

        // Intercept the window close button: hide to tray unless we're really
        // quitting or there's no tray to hide into.
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.quit || self._tray.is_none() {
                self.shared.request_stop();
                // allow the close to proceed
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hide_window(ctx);
            }
        }

        self.sample_rates();
        self.reap_worker();

        egui::CentralPanel::default().show(ctx, |ui| self.draw(ui));

        // Keep the counters live and tray actions responsive.
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl App {
    fn draw(&mut self, ui: &mut egui::Ui) {
        let state = self.shared.state();
        let admin = self.shared.is_admin();

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::Image::new(egui::load::SizedTexture::new(
                    self.logo.id(),
                    egui::vec2(36.0, 36.0),
                ))
                .rounding(8.0),
            );
            ui.add_space(6.0);
            ui.heading("RickyNet");
        });
        ui.separator();

        // --- State indicator ---
        let (dot, text) = match state {
            ConnState::Disconnected => (GRAY, "Disconnected".to_string()),
            ConnState::Connecting => (AMBER, "Connecting…".to_string()),
            ConnState::Connected => (GREEN, "Connected".to_string()),
            ConnState::Error => (RED, format!("Error: {}", self.shared.error())),
        };
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 7.0, dot);
            if state == ConnState::Connecting {
                ui.add(egui::Spinner::new().size(16.0));
            }
            ui.label(RichText::new(text).strong());
        });

        // --- Big Connect/Disconnect button ---
        ui.add_space(10.0);
        let (label, color, enabled) = match state {
            ConnState::Disconnected | ConnState::Error => ("Connect", GREEN, admin),
            ConnState::Connecting => ("Connecting…", AMBER, false),
            ConnState::Connected => ("Disconnect", RED, true),
        };
        let btn = egui::Button::new(RichText::new(label).size(18.0).strong().color(Color32::WHITE))
            .fill(color)
            .min_size(egui::vec2(ui.available_width(), 46.0));
        if ui.add_enabled(enabled, btn).clicked() {
            self.toggle_connection();
        }

        if !admin {
            ui.add_space(4.0);
            ui.colored_label(
                RED,
                "Administrator required — restart and accept the UAC prompt.",
            );
        }

        // --- Transport selector ---
        ui.add_space(12.0);
        ui.add_enabled_ui(!self.busy(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Link:");
                ui.selectable_value(&mut self.transport, TransportKind::Usb, "USB");
                ui.selectable_value(&mut self.transport, TransportKind::Wifi, "Wi-Fi");
            });
            if self.transport == TransportKind::Wifi {
                ui.horizontal(|ui| {
                    ui.label("Phone IP:");
                    ui.text_edit_singleline(&mut self.phone_ip);
                });
            }
            ui.horizontal(|ui| {
                ui.label("Port:");
                ui.add(egui::TextEdit::singleline(&mut self.port_str).desired_width(70.0));
            });
        });

        // --- Counters ---
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        egui::Grid::new("counters")
            .num_columns(2)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                ui.label(RichText::new("↓ Down").color(Color32::from_rgb(52, 152, 219)));
                ui.label(format!(
                    "{}   ({}/s)",
                    human_bytes(self.shared.rx()),
                    human_bytes(self.rate_rx as u64)
                ));
                ui.end_row();
                ui.label(RichText::new("↑ Up").color(Color32::from_rgb(155, 89, 182)));
                ui.label(format!(
                    "{}   ({}/s)",
                    human_bytes(self.shared.tx()),
                    human_bytes(self.rate_tx as u64)
                ));
                ui.end_row();
            });

        // --- Status log ---
        ui.add_space(8.0);
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height(96.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.shared.logs() {
                    ui.label(RichText::new(line).small().monospace());
                }
            });
    }
}

fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Build the tray icon + menu and wire events to the shared action queue. The
/// event handlers wake the egui context so the UI processes them even while
/// hidden. Returns None if the platform can't create a tray (non-fatal).
fn build_tray(ctx: egui::Context, actions: Arc<Mutex<VecDeque<TrayAction>>>) -> Option<TrayIcon> {
    let tray_icon = icon::tray_icon()?;

    let menu = Menu::new();
    let show = MenuItem::with_id("rn_show", "Show RickyNet", true, None);
    let toggle = MenuItem::with_id("rn_toggle", "Connect / Disconnect", true, None);
    let sep = PredefinedMenuItem::separator();
    let quit = MenuItem::with_id("rn_quit", "Quit", true, None);
    menu.append(&show).ok()?;
    menu.append(&toggle).ok()?;
    menu.append(&sep).ok()?;
    menu.append(&quit).ok()?;

    let show_id = show.id().clone();
    let toggle_id = toggle.id().clone();
    let quit_id = quit.id().clone();

    {
        let actions = actions.clone();
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            let action = if ev.id == show_id {
                TrayAction::Show
            } else if ev.id == toggle_id {
                TrayAction::ConnectDisconnect
            } else if ev.id == quit_id {
                TrayAction::Quit
            } else {
                return;
            };
            actions.lock().unwrap().push_back(action);
            ctx.request_repaint();
        }));
    }
    {
        let actions = actions.clone();
        let ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                ..
            } = ev
            {
                actions.lock().unwrap().push_back(TrayAction::ToggleWindow);
                ctx.request_repaint();
            }
        }));
    }

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("RickyNet")
        .with_icon(tray_icon)
        .build()
        .ok()
}
