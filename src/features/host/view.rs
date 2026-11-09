//! Studio Host & QuickSupport UI (`src/features/host/view.rs`):
//! - "Share this computer" control center (6-digit session code + 4-digit E2EE PIN generator)
//! - QuickSupport E2EE Viewer connection card (connect via Code + PIN + Repeater Mode II)
//! - Permanent unhideable active-session security banner with instant red "Terminate Now" button
//! - Real-time anti-RAT audit log and virtual drive (`M:\`, `D:\`, `H:\`, `V:\`) status inspector

use crate::shared::i18n::{fill, tr};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, Order, RichText, Stroke, Ui};
use lucide_icons::Icon;

use super::ft_server::VirtualDrives;
use super::session::{HostConfig, register_repeater_mode_ii, serve_e2ee_stream, serve_stream};
use crate::features::connection::{ConnectRequest, Credentials, ServerAddress, SessionOptions};
use crate::shared::crypto::{
    BruteForceLimiter, format_code, generate_pin, generate_session_code, normalize_code,
};
use crate::shared::theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStatus {
    Idle,
    Waiting { endpoint: String, e2ee: bool },
    Active { peer: String, e2ee: bool },
    Locked { reason: String },
}

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub timestamp: String,
    pub message: String,
    pub is_alert: bool,
}

pub enum HostUiEvent {
    Waiting {
        endpoint: String,
        e2ee: bool,
    },
    Connected {
        peer: String,
        e2ee: bool,
    },
    Disconnected {
        peer: String,
        reason: String,
    },
    PinFailed {
        peer: String,
        attempts: u32,
        locked: bool,
    },
    Stopped(String),
}

pub struct HostView {
    /// Selected sharing mode: `true` = QuickSupport E2EE (PIN), `false` = Local Studio Password.
    pub use_e2ee_pin: bool,
    pub use_repeater: bool,
    pub bind_addr: String,
    pub repeater_addr: String,
    pub session_code: String,
    pub pin: String,
    pub studio_password: String,
    pub view_only: bool,
    pub status: HostStatus,
    pub audit_log: Vec<AuditEntry>,
    pub drives: VirtualDrives,
    copied_until: Option<Instant>,

    // QuickSupport Viewer Connect card fields (for the operator connecting to a remote host)
    pub viewer_target_code: String,
    pub viewer_pin: String,
    pub viewer_repeater: String,
    pub viewer_view_only: bool,
    /// Direct `host:port` to dial instead of a session code (LAN, no repeater).
    pub viewer_direct: String,
    pub join_open: bool,
    show_pin: bool,
    join_fresh: bool,
    prefs_loaded: bool,
    /// Last session code written to the saved preferences.
    saved_code: String,
    /// Set when a pasted invitation filled the dialog, so it can say so.
    pub pasted_invitation: bool,

    // Session bookkeeping for the Share page
    session_started: Option<Instant>,
    last_session: Option<(String, u64)>,
    failed_pins: u32,

    // Background Host thread control handles
    stop_flag: Arc<AtomicBool>,
    active_stream: Arc<Mutex<Option<TcpStream>>>,
    events_rx: Option<Receiver<HostUiEvent>>,
}

impl Default for HostView {
    fn default() -> Self {
        Self {
            use_e2ee_pin: true,
            use_repeater: false,
            bind_addr: String::from("0.0.0.0:5950"),
            repeater_addr: String::from("127.0.0.1:5500"),
            session_code: generate_session_code(),
            pin: generate_pin(),
            studio_password: String::from("studio"),
            view_only: false,
            status: HostStatus::Idle,
            audit_log: Vec::new(),
            drives: VirtualDrives::default(),
            copied_until: None,
            viewer_target_code: String::new(),
            viewer_pin: String::new(),
            viewer_repeater: String::new(),
            viewer_view_only: false,
            viewer_direct: String::new(),
            join_open: false,
            show_pin: false,
            join_fresh: true,
            prefs_loaded: false,
            saved_code: String::new(),
            pasted_invitation: false,
            session_started: None,
            last_session: None,
            failed_pins: 0,
            stop_flag: Arc::new(AtomicBool::new(false)),
            active_stream: Arc::new(Mutex::new(None)),
            events_rx: None,
        }
    }
}

/// What a copied invitation carries. Read from either language, so an invitation made on a Spanish
/// computer works on an English one.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Invitation {
    pub code: Option<String>,
    pub pin: Option<String>,
    pub repeater: Option<String>,
    pub address: Option<String>,
}

pub fn parse_invitation(text: &str) -> Option<Invitation> {
    let mut inv = Invitation::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_lowercase(), value.trim());
        let digits = |v: &str| {
            v.split_whitespace()
                .next()
                .map(|t| t.chars().filter(char::is_ascii_digit).collect::<String>())
                .filter(|d| !d.is_empty())
        };
        match key.as_str() {
            "session code" | "código de sesión" | "codigo de sesion" => inv.code = digits(value),
            "e2ee pin" | "pin e2ee" | "pin" => inv.pin = digits(value),
            "repeater" | "repetidor" => inv.repeater = Some(value.to_string()),
            "address" | "dirección" | "direccion" => inv.address = Some(value.to_string()),
            _ => {}
        }
    }
    (inv.code.is_some() && inv.pin.is_some() || inv.address.is_some() && inv.pin.is_some())
        .then_some(inv)
}

const KEY_VIEWER_REPEATER: &str = "lucarna_viewer_repeater";
const KEY_HOST_REPEATER: &str = "lucarna_host_repeater";
const KEY_HOST_CODE: &str = "lucarna_host_code";

impl HostView {
    /// Repeaters are remembered, so the second time nobody has to type one.
    pub fn load_prefs(&mut self, ctx: &egui::Context) {
        if self.prefs_loaded {
            return;
        }
        self.prefs_loaded = true;
        let get = |key: &str| ctx.data_mut(|d| d.get_persisted::<String>(egui::Id::NULL.with(key)));
        if let Some(r) = get(KEY_VIEWER_REPEATER) {
            self.viewer_repeater = r;
        }
        if let Some(r) = get(KEY_HOST_REPEATER) {
            self.repeater_addr = r;
        }
        // The code is an address, not a secret, so it stays the same between launches; the PIN does not.
        if let Some(c) =
            get(KEY_HOST_CODE).filter(|c| c.len() == 6 && c.chars().all(|d| d.is_ascii_digit()))
        {
            self.saved_code = c.clone();
            self.session_code = c;
        }
    }

    fn save_pref(ctx: &egui::Context, key: &str, value: &str) {
        ctx.data_mut(|d| d.insert_persisted(egui::Id::NULL.with(key), value.to_string()));
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.status,
            HostStatus::Waiting { .. } | HostStatus::Active { .. }
        )
    }

    pub fn regenerate_credentials(&mut self) {
        self.session_code = generate_session_code();
        self.pin = generate_pin();
        if matches!(self.status, HostStatus::Locked { .. }) {
            self.status = HostStatus::Idle;
        }
        self.push_audit(
            fill(
                tr("Generated new session code {} and 4-digit E2EE PIN."),
                &[&(format_code(&self.session_code))],
            ),
            false,
        );
    }

    pub fn format_invitation(&self) -> String {
        let code_fmt = format_code(&self.session_code);
        if self.use_repeater {
            fill(
                tr(
                    "Lucarna QuickSupport Invitation\nRepeater: {}\nSession Code: {} (ID:{})\nE2EE PIN: {}",
                ),
                &[
                    &(self.repeater_addr.trim()),
                    &code_fmt,
                    &(normalize_code(&self.session_code)),
                    &self.pin,
                ],
            )
        } else if self.use_e2ee_pin {
            fill(
                tr("Lucarna Studio E2EE Invitation\nAddress: {}\nSession Code: {}\nE2EE PIN: {}"),
                &[&(self.bind_addr.trim()), &code_fmt, &self.pin],
            )
        } else {
            fill(
                tr("Lucarna Studio Host\nAddress: {}\nPassword: {}"),
                &[&(self.bind_addr.trim()), &self.studio_password],
            )
        }
    }

    /// Builds a `ConnectRequest` from the QuickSupport operator card.
    pub fn build_viewer_connect_request(&self) -> Option<ConnectRequest> {
        let raw_target = if self.viewer_direct.trim().is_empty() {
            self.viewer_target_code.trim()
        } else {
            self.viewer_direct.trim()
        };
        if raw_target.is_empty() {
            return None;
        }
        let rep_trimmed = self.viewer_repeater.trim();
        let repeater = if rep_trimmed.is_empty() {
            None
        } else {
            ServerAddress::parse(rep_trimmed)
        };

        // If a repeater is used and the user typed a 6-9 digit code (e.g. "582-914"), format as "ID:582914"
        let normalized = normalize_code(raw_target);
        let addr_str = if repeater.is_some()
            && !raw_target.contains(':')
            && !raw_target.contains('.')
            && (6..=9).contains(&normalized.len())
            && normalized.chars().all(|c| c.is_ascii_digit())
        {
            format!("ID:{normalized}")
        } else {
            raw_target.to_string()
        };

        let address = ServerAddress::parse(&addr_str)?;
        let options = SessionOptions {
            view_only: self.viewer_view_only,
            ..Default::default()
        };

        Some(ConnectRequest {
            address,
            repeater,
            ssh: String::new(),
            creds: Credentials {
                user: String::new(),
                password: String::new(),
                pin: self.viewer_pin.trim().to_string(),
            },
            options,
        })
    }

    pub fn start_sharing(&mut self, ctx: &egui::Context) {
        self.stop_sharing();
        self.failed_pins = 0;
        self.stop_flag = Arc::new(AtomicBool::new(false));
        self.active_stream = Arc::new(Mutex::new(None));

        let (tx, rx) = mpsc::channel();
        self.events_rx = Some(rx);

        let stop_flag = self.stop_flag.clone();
        let active_stream = self.active_stream.clone();
        let use_repeater = self.use_repeater;
        if use_repeater {
            Self::save_pref(ctx, KEY_HOST_REPEATER, self.repeater_addr.trim());
        }
        let use_e2ee = self.use_e2ee_pin || self.use_repeater;
        let bind_addr = self.bind_addr.trim().to_string();
        let repeater_addr = self.repeater_addr.trim().to_string();
        let session_code = normalize_code(&self.session_code);
        let pin = self.pin.trim().to_string();
        let password = if use_e2ee {
            String::new()
        } else {
            self.studio_password.clone()
        };
        let view_only = self.view_only;
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let config = HostConfig {
                desktop_name: tr("Lucarna Studio Host").into(),
                password,
                view_only,
                fixed_challenge: None,
            };
            if use_repeater {
                run_repeater_host_loop(
                    repeater_addr,
                    session_code,
                    pin,
                    config,
                    stop_flag,
                    active_stream,
                    tx,
                    ctx,
                );
            } else {
                run_listener_host_loop(
                    bind_addr,
                    use_e2ee,
                    pin,
                    config,
                    stop_flag,
                    active_stream,
                    tx,
                    ctx,
                );
            }
        });
    }

    pub fn stop_sharing(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(stream) = self.active_stream.lock().unwrap().take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if self.is_running() {
            self.status = HostStatus::Idle;
            self.push_audit("Sharing stopped immediately by local user.".into(), false);
        }
        self.events_rx = None;
    }

    pub fn poll_events(&mut self) {
        let mut incoming = Vec::new();
        if let Some(rx) = &self.events_rx {
            while let Ok(ev) = rx.try_recv() {
                incoming.push(ev);
            }
        }
        for ev in incoming {
            match ev {
                HostUiEvent::Waiting { endpoint, e2ee } => {
                    self.status = HostStatus::Waiting {
                        endpoint: endpoint.clone(),
                        e2ee,
                    };
                    self.push_audit(
                        fill(
                            tr("Waiting for remote connection on {} ({})."),
                            &[
                                &endpoint,
                                &(if e2ee {
                                    tr("E2EE ChaCha20-Poly1305")
                                } else {
                                    tr("RFB 3.8")
                                }),
                            ],
                        ),
                        false,
                    );
                }
                HostUiEvent::Connected { peer, e2ee } => {
                    self.session_started = Some(Instant::now());
                    self.status = HostStatus::Active {
                        peer: peer.clone(),
                        e2ee,
                    };
                    self.push_audit(
                        fill(
                            tr("Session established with {} ({})."),
                            &[
                                &peer,
                                &(if e2ee {
                                    tr("Verified E2EE PIN")
                                } else {
                                    tr("VNC Auth")
                                }),
                            ],
                        ),
                        false,
                    );
                }
                HostUiEvent::Disconnected { peer, reason } => {
                    if let Some(t) = self.session_started.take() {
                        self.last_session = Some((peer.clone(), t.elapsed().as_secs()));
                    }
                    self.push_audit(
                        fill(tr("Session ended with {}: {}"), &[&peer, &reason]),
                        false,
                    );
                }
                HostUiEvent::PinFailed {
                    peer,
                    attempts,
                    locked,
                } => {
                    self.failed_pins += 1;
                    self.push_audit(
                        fill(
                            tr("SECURITY ALERT: Failed E2EE PIN attempt #{} from {}."),
                            &[&attempts, &peer],
                        ),
                        true,
                    );
                    if locked {
                        let reason = fill(
                            tr(
                                "Locked after {} failed PIN attempts from {}. Regenerate code to resume.",
                            ),
                            &[&attempts, &peer],
                        );
                        self.status = HostStatus::Locked {
                            reason: reason.clone(),
                        };
                        self.push_audit(reason, true);
                        self.events_rx = None;
                    }
                }
                HostUiEvent::Stopped(reason) => {
                    if !matches!(self.status, HostStatus::Locked { .. }) {
                        self.status = HostStatus::Idle;
                    }
                    self.push_audit(reason, false);
                    self.events_rx = None;
                }
            }
        }
    }

    fn push_audit(&mut self, message: String, is_alert: bool) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        self.audit_log.insert(
            0,
            AuditEntry {
                timestamp: format!("{h:02}:{m:02}:{s:02}"),
                message,
                is_alert,
            },
        );
        if self.audit_log.len() > 50 {
            self.audit_log.truncate(50);
        }
    }

    /// Permanent, unhideable banner while this computer is shared: its own always-on-top window, so it stays
    /// visible over whatever the remote person is doing, with a red button that ends the session at once.
    pub fn show_active_banner(&mut self, ctx: &egui::Context) {
        self.poll_events();
        self.load_prefs(ctx);
        if self.saved_code != self.session_code {
            Self::save_pref(ctx, KEY_HOST_CODE, &self.session_code);
            self.saved_code = self.session_code.clone();
        }
        if !self.is_running() {
            return;
        }
        let (is_active, headline, e2ee) = match &self.status {
            HostStatus::Active { peer, e2ee } => (
                true,
                fill(tr("Assistance session active with {}"), &[&peer]),
                *e2ee,
            ),
            HostStatus::Waiting { endpoint, e2ee } => (
                false,
                fill(tr("Sharing this computer · waiting on {}"), &[&endpoint]),
                *e2ee,
            ),
            _ => return,
        };
        let sub = format!(
            "{} · {}",
            if e2ee {
                tr("Encrypted end to end")
            } else {
                tr("Not encrypted")
            },
            self.elapsed_label()
                .unwrap_or_else(|| tr("nobody connected yet").into())
        );
        let border = if is_active {
            Color32::from_rgb(0xf8, 0x71, 0x71)
        } else {
            theme::ACCENT
        };
        let chips = [
            (Icon::Eye, tr("Screen")),
            (
                Icon::MousePointer2,
                if self.view_only {
                    tr("View only")
                } else {
                    tr("Control")
                },
            ),
            (Icon::Folder, tr("Files")),
        ];
        let width = 780.0;
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Lucarna is sharing this computer")
            .with_always_on_top()
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_inner_size([width, 76.0]);
        if let Some(mon) = ctx.input(|i| i.viewport().monitor_size) {
            builder = builder.with_position([(mon.x - width) / 2.0, 40.0]);
        }
        let mut stop = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("lucarna_host_banner"),
            builder,
            |ctx, _class| {
                let dark = &theme::DARK;
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(dark.bg)
                            .stroke(Stroke::new(2.0, border))
                            .inner_margin(egui::Margin::symmetric(18, 10)),
                    )
                    .show(ctx, |ui| {
                        ui.visuals_mut().override_text_color = Some(dark.text);
                        ui.horizontal_centered(|ui| {
                            let pulse = ((ui.input(|i| i.time) * 3.0).sin() * 0.3 + 0.7) as f32;
                            let (r, _) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                r.center(),
                                5.0,
                                border.gamma_multiply(pulse),
                            );
                            ui.ctx().request_repaint_after(Duration::from_millis(250));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&headline).strong().size(14.0));
                                ui.label(RichText::new(&sub).size(12.0).color(dark.text2));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let label = if is_active {
                                    tr("End session now")
                                } else {
                                    tr("Stop sharing")
                                };
                                let btn = egui::Button::new(
                                    RichText::new(format!("{}  {label}", char::from(Icon::Power)))
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(Color32::from_rgb(0xdc, 0x26, 0x26))
                                .min_size(egui::vec2(0.0, 40.0));
                                if ui
                                    .add(btn)
                                    .on_hover_text(tr(
                                        "Disconnect the remote person and stop sharing",
                                    ))
                                    .clicked()
                                {
                                    stop = true;
                                }
                                for (icon, text) in chips.iter().rev() {
                                    theme::tag(ui, *icon, text, dark.text, dark.card);
                                }
                            });
                        });
                    });
            },
        );
        if stop {
            self.stop_sharing();
        }
    }

    /// `mm:ss` since the remote person connected, once someone is.
    fn elapsed_label(&self) -> Option<String> {
        let s = self.session_started?.elapsed().as_secs();
        Some(format!("{:02}:{:02}", s / 60, s % 60))
    }

    fn stage(&self) -> Stage {
        match &self.status {
            HostStatus::Idle => Stage::Idle,
            HostStatus::Waiting { .. } => Stage::Waiting,
            HostStatus::Active { .. } => Stage::Active,
            HostStatus::Locked { .. } => Stage::Locked,
        }
    }

    /// The "Share this device" page: one card that walks through the session, and the rules on the right.
    pub fn show_page(&mut self, ui: &mut Ui) {
        self.poll_events();
        self.load_prefs(&ui.ctx().clone());
        let p = theme::palette(ui.ctx());
        let stage = self.stage();
        if stage == Stage::Active {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        egui::ScrollArea::vertical()
            .id_salt("share_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(48, 32))
                    .show(ui, |ui| {
                        let side_w = 360.0;
                        let main_w = (ui.available_width() - side_w - 24.0).clamp(420.0, 780.0);
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(main_w);
                                egui::Frame::NONE
                                    .fill(p.card)
                                    .stroke(Stroke::new(1.0, p.border))
                                    .corner_radius(CornerRadius::same(18))
                                    .show(ui, |ui| {
                                        ui.set_width(main_w);
                                        if stage != Stage::Locked {
                                            self.stepper(ui, stage);
                                            ui.add(egui::Separator::default().spacing(0.0));
                                        }
                                        egui::Frame::NONE
                                            .inner_margin(egui::Margin::symmetric(40, 32))
                                            .show(ui, |ui| {
                                                ui.set_width(main_w - 80.0);
                                                match stage {
                                                    Stage::Idle => self.idle_view(ui),
                                                    Stage::Waiting => self.waiting_view(ui),
                                                    Stage::Active => self.active_view(ui),
                                                    Stage::Locked => self.locked_view(ui),
                                                }
                                            });
                                    });
                            });
                            ui.add_space(24.0);
                            ui.vertical(|ui| {
                                ui.set_width(side_w);
                                self.permissions_card(ui);
                                ui.add_space(16.0);
                                safety_card(ui);
                            });
                        });
                    });
            });
    }

    fn stepper(&self, ui: &mut Ui, stage: Stage) {
        let p = theme::palette(ui.ctx());
        let current = match stage {
            Stage::Idle => -1,
            Stage::Waiting => 0,
            Stage::Active => 2,
            Stage::Locked => -1,
        };
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(28, 16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let names = [tr("Waiting"), tr("Connecting"), tr("Active"), tr("Ended")];
                    for (i, name) in names.iter().enumerate() {
                        let i = i as i32;
                        let done = i < current;
                        let now = i == current;
                        let (bg, fg, ring) = if done {
                            (theme::ACCENT, theme::ON_ACCENT, theme::ACCENT)
                        } else if now {
                            (p.accent_soft, p.accent_text, theme::ACCENT)
                        } else {
                            (Color32::TRANSPARENT, p.text3, p.border_strong)
                        };
                        let (r, _) =
                            ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                        ui.painter()
                            .circle(r.center(), 11.5, bg, Stroke::new(1.5, ring));
                        ui.painter().text(
                            r.center(),
                            egui::Align2::CENTER_CENTER,
                            if done {
                                "✓".to_string()
                            } else {
                                (i + 1).to_string()
                            },
                            egui::FontId::proportional(12.0),
                            fg,
                        );
                        ui.label(
                            RichText::new(*name)
                                .size(13.0)
                                .strong()
                                .color(if done || now { p.text } else { p.text3 }),
                        );
                        if i < 3 {
                            let w = ((ui.available_width() - (3 - i) as f32 * 120.0) / 1.0)
                                .clamp(16.0, 400.0);
                            let (r, _) = ui.allocate_exact_size(
                                egui::vec2(w.min(90.0), 2.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(
                                r,
                                1.0,
                                if done { theme::ACCENT } else { p.border },
                            );
                        }
                    }
                });
            });
    }

    /// The session code and the PIN, big, with a button for a new pair. Shown before and while sharing.
    fn code_cards(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui.ctx());
        ui.columns(2, |cols| {
            code_card(
                &mut cols[0],
                tr("Session code"),
                &format_code(&self.session_code),
                p.text,
                None,
            );
            let mut refresh = false;
            code_card(
                &mut cols[1],
                tr("PIN"),
                &self.pin,
                p.accent_text,
                Some(&mut refresh),
            );
            if refresh {
                self.regenerate_credentials();
            }
        });
    }

    fn copy_invitation_button(&mut self, ui: &mut Ui, primary: bool, width: f32) {
        let copied = self.copied_until.is_some_and(|t| Instant::now() < t);
        let label = if copied {
            fill(tr("{}  Invitation copied"), &[&(char::from(Icon::Check))])
        } else {
            fill(tr("{}  Copy invitation"), &[&(char::from(Icon::Copy))])
        };
        let size = egui::vec2(width, 48.0);
        let clicked = if primary {
            ui.add(theme::primary(label).min_size(size)).clicked()
        } else {
            ui.add(egui::Button::new(label).min_size(size)).clicked()
        };
        if clicked {
            ui.ctx().copy_text(self.format_invitation());
            self.copied_until = Some(Instant::now() + Duration::from_secs(2));
        }
    }

    /// First thing on the page, like any remote-support tool: your code and PIN, ready to read out.
    fn idle_view(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, p.text3);
            ui.label(
                RichText::new(tr("Ready. Nothing is shared until you start."))
                    .size(15.0)
                    .strong(),
            );
        });
        ui.add_space(18.0);
        self.code_cards(ui);
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            let go = theme::primary(fill(
                tr("Start sharing  {}"),
                &[&(char::from(Icon::ArrowRight))],
            ))
            .min_size(egui::vec2((ui.available_width() - 210.0).max(200.0), 48.0));
            if ui.add(go).clicked() {
                let ctx = ui.ctx().clone();
                self.start_sharing(&ctx);
            }
            self.copy_invitation_button(ui, false, 200.0);
        });
        ui.add_space(10.0);
        ui.label(
            RichText::new(tr("The code and PIN only work while you are sharing."))
                .size(12.0)
                .color(p.text3),
        );
        ui.add_space(18.0);
        ui.add(egui::Separator::default().spacing(0.0));
        ui.add_space(14.0);
        ui.label(RichText::new(tr("How they reach you")).size(13.0).strong());
        ui.add_space(8.0);
        self.reach_options(ui);
    }

    /// Direct (LAN) or through a repeater, and the address that goes with it.
    fn reach_options(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            if theme::chip(ui, Icon::Network, tr("Direct (LAN)"), !self.use_repeater).clicked() {
                self.use_repeater = false;
            }
            if theme::chip(
                ui,
                Icon::Globe,
                tr("Repeater (through NAT)"),
                self.use_repeater,
            )
            .clicked()
            {
                self.use_repeater = true;
            }
        });
        ui.add_space(10.0);
        let (label, field, hint) = if self.use_repeater {
            (
                tr("Repeater"),
                &mut self.repeater_addr,
                "repeater.example.com:5500",
            )
        } else {
            (tr("Listen on"), &mut self.bind_addr, "0.0.0.0:5950")
        };
        ui.label(RichText::new(label).size(12.0).strong().color(p.text2));
        ui.add(
            egui::TextEdit::singleline(field)
                .font(egui::FontId::monospace(14.0))
                .hint_text(hint)
                .margin(egui::vec2(14.0, 11.0))
                .desired_width(f32::INFINITY),
        );
        if !self.use_repeater {
            ui.add_space(6.0);
            ui.checkbox(
                &mut self.use_e2ee_pin,
                tr("Require the PIN and encrypt the session (recommended)"),
            );
            if !self.use_e2ee_pin {
                ui.label(
                    RichText::new(tr("Classic VNC password"))
                        .size(12.0)
                        .color(p.text2),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.studio_password)
                        .password(true)
                        .desired_width(f32::INFINITY),
                );
            }
        }
    }

    fn waiting_view(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui.ctx());
        let endpoint = match &self.status {
            HostStatus::Waiting { endpoint, .. } => endpoint.clone(),
            _ => String::new(),
        };
        ui.horizontal(|ui| {
            let pulse = ((ui.input(|i| i.time) * 3.0).sin() * 0.3 + 0.7) as f32;
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(r.center(), 5.0, theme::ACCENT.gamma_multiply(pulse));
            ui.ctx().request_repaint_after(Duration::from_millis(250));
            ui.label(
                RichText::new(tr("Waiting for the other person"))
                    .size(15.0)
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(fill(tr("via {}"), &[&endpoint]))
                        .monospace()
                        .size(12.0)
                        .color(p.text3),
                );
            });
        });
        ui.add_space(18.0);
        self.code_cards(ui);
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 150.0).max(200.0);
            self.copy_invitation_button(ui, true, w);
            if ui
                .add(egui::Button::new(tr("Stop waiting")).min_size(egui::vec2(0.0, 48.0)))
                .clicked()
            {
                self.stop_sharing();
            }
        });
        ui.add_space(14.0);
        egui::Frame::NONE
            .fill(p.input_bg)
            .stroke(Stroke::new(1.0, p.border_strong))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(self.format_invitation())
                        .monospace()
                        .size(12.0)
                        .color(p.text2),
                );
            });
        ui.add_space(10.0);
        ui.label(
            RichText::new(tr(
                "The code ends when you stop waiting. Three wrong PINs revoke it.",
            ))
            .size(12.0)
            .color(p.text3),
        );
        self.last_session_note(ui);
    }

    fn active_view(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui.ctx());
        let (peer, e2ee) = match &self.status {
            HostStatus::Active { peer, e2ee } => (peer.clone(), *e2ee),
            _ => (String::new(), false),
        };
        egui::Frame::NONE
            .fill(p.success.gamma_multiply(0.12))
            .stroke(Stroke::new(1.0, p.success.gamma_multiply(0.45)))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(egui::Margin::symmetric(20, 16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    icon_tile(ui, Icon::Monitor, 44.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(fill(tr("Session active with {}"), &[&peer]))
                                .size(17.0)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                if e2ee {
                                    tr("Encrypted end to end")
                                } else {
                                    tr("Not encrypted")
                                },
                                self.elapsed_label().unwrap_or_default()
                            ))
                            .size(13.0)
                            .color(p.text2),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let btn = egui::Button::new(
                            RichText::new(fill(
                                tr("{}  End session now"),
                                &[&(char::from(Icon::Power))],
                            ))
                            .strong()
                            .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(0xdc, 0x26, 0x26))
                        .min_size(egui::vec2(0.0, 46.0));
                        if ui.add(btn).clicked() {
                            self.stop_sharing();
                        }
                    });
                });
            });
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 24.0) / 3.0;
            let control = if self.view_only {
                tr("View only")
            } else {
                tr("Controlling")
            };
            for (icon, title, note) in [
                (
                    Icon::Eye,
                    tr("Screen shared").to_string(),
                    tr("Always on while sharing").to_string(),
                ),
                (
                    Icon::MousePointer2,
                    control.to_string(),
                    tr("Paused when you move").to_string(),
                ),
                (
                    Icon::Folder,
                    tr("Files").to_string(),
                    fill(tr("{} shared folders"), &[&(self.drives.roots().len())]),
                ),
            ] {
                egui::Frame::NONE
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(w - 30.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(char::from(icon).to_string()).color(p.text2));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(title).size(13.0).strong());
                                ui.label(RichText::new(note).size(12.0).color(p.text3));
                            });
                        });
                    });
            }
        });
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Activity")).size(14.0).strong());
            theme::count_pill(ui, self.audit_log.len());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !self.audit_log.is_empty() && ui.small_button(tr("Clear")).clicked() {
                    self.audit_log.clear();
                }
            });
        });
        ui.add_space(8.0);
        for entry in self.audit_log.iter().take(6) {
            egui::Frame::NONE
                .fill(p.surface)
                .stroke(Stroke::new(
                    1.0,
                    if entry.is_alert {
                        p.danger.gamma_multiply(0.5)
                    } else {
                        p.border
                    },
                ))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&entry.timestamp)
                                .monospace()
                                .size(11.5)
                                .color(p.text3),
                        );
                        ui.label(
                            RichText::new(&entry.message)
                                .size(12.5)
                                .color(if entry.is_alert { p.danger } else { p.text }),
                        );
                    });
                });
            ui.add_space(6.0);
        }
    }

    fn locked_view(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui.ctx());
        let reason = match &self.status {
            HostStatus::Locked { reason } => reason.clone(),
            _ => String::new(),
        };
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            icon_tile(ui, Icon::ShieldAlert, 64.0);
            ui.add_space(12.0);
            ui.label(
                RichText::new(tr("Sharing is locked"))
                    .size(26.0)
                    .strong()
                    .color(p.danger),
            );
            ui.add_space(6.0);
            ui.label(RichText::new(reason).size(14.0).color(p.text2));
            ui.add_space(18.0);
            let btn = theme::primary(fill(
                tr("{}  Generate a new code"),
                &[&(char::from(Icon::RefreshCw))],
            ))
            .min_size(egui::vec2(240.0, 48.0));
            if ui.add(btn).clicked() {
                self.regenerate_credentials();
            }
        });
        self.last_session_note(ui);
    }

    fn last_session_note(&self, ui: &mut Ui) {
        let p = theme::palette(ui.ctx());
        if let Some((peer, secs)) = &self.last_session {
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!(
                    "Last session: {peer}, {} min {:02} s. {} failed PIN attempts so far.",
                    secs / 60,
                    secs % 60,
                    self.failed_pins
                ))
                .size(12.0)
                .color(p.text3),
            );
        }
    }

    fn permissions_card(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui.ctx());
        let running = self.is_running();
        side_card(ui, |ui| {
            ui.label(RichText::new(tr("What they can do")).size(15.0).strong());
            ui.add_space(8.0);
            let mut control = !self.view_only;
            perm_row(
                ui,
                Icon::Eye,
                tr("See my screen"),
                tr("Always on while sharing"),
                &mut true,
                false,
            );
            if perm_row(
                ui,
                Icon::MousePointer2,
                tr("Control mouse and keyboard"),
                tr("Your mouse always wins"),
                &mut control,
                !running,
            ) {
                self.view_only = !control;
            }
            let mut files = true;
            perm_row(
                ui,
                Icon::Folder,
                tr("Transfer files"),
                tr("Only these folders"),
                &mut files,
                false,
            );
            ui.horizontal_wrapped(|ui| {
                ui.add_space(30.0);
                for (letter, path) in self.drives.roots() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| tr("Volumes").into());
                    egui::Frame::NONE
                        .fill(p.elevated)
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("{letter}: {name}"))
                                    .size(11.0)
                                    .strong()
                                    .color(p.text2),
                            );
                        });
                }
            });
        });
    }

    /// Dialog "Connect with a code" for the person helping: code and PIN as boxes, repeater tucked away.
    pub fn show_join_dialog(&mut self, ctx: &egui::Context) -> Option<ConnectRequest> {
        if !self.join_open {
            self.join_fresh = true;
            self.pasted_invitation = false;
            return None;
        }
        self.load_prefs(ctx);
        // A whole invitation pasted with Ctrl/Cmd+V fills every field at once.
        let pasted = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Paste(t) => parse_invitation(t),
                _ => None,
            })
        });
        if let Some(inv) = pasted {
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Paste(_))));
            self.viewer_target_code = inv.code.unwrap_or_default();
            self.viewer_pin = inv.pin.unwrap_or_default();
            self.viewer_repeater = inv.repeater.unwrap_or_default();
            self.viewer_direct = inv.address.unwrap_or_default();
            self.pasted_invitation = true;
        }
        let autofocus = std::mem::take(&mut self.join_fresh);
        let p = theme::palette(ctx);
        let mut request = None;
        let mut close = false;
        let frame = egui::Frame::window(&ctx.global_style())
            .fill(p.card)
            .inner_margin(egui::Margin::ZERO)
            .corner_radius(CornerRadius::same(16));
        egui::Window::new("join_with_code")
            .title_bar(false)
            .frame(frame)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(Order::Foreground)
            .fixed_size([520.0, 0.0])
            .show(ctx, |ui| {
                close = theme::window_header(ui, Icon::Lock, tr("Connect with a code"), |_| {});
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(24, 18))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if self.pasted_invitation {
                            ui.label(
                                RichText::new(format!(
                                    "{} {}",
                                    char::from(Icon::Check),
                                    tr("Invitation read. Check it and press Connect.")
                                ))
                                .size(13.0)
                                .strong()
                                .color(p.success),
                            );
                        } else {
                            let paste = if cfg!(target_os = "macos") {
                                "⌘V"
                            } else {
                                "Ctrl+V"
                            };
                            ui.label(
                                RichText::new(fill(
                                    tr("Ask the other person to copy their invitation, then paste it here with {}. Or type the code and PIN."),
                                    &[&paste],
                                ))
                                .size(13.0)
                                .color(p.text2),
                            );
                        }
                        ui.add_space(16.0);
                        ui.label(RichText::new(tr("Session code")).size(12.0).strong().color(p.text2));
                        ui.add_space(6.0);
                        digit_boxes(
                            ui,
                            "join_code",
                            &mut self.viewer_target_code,
                            6,
                            false,
                            autofocus,
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr("PIN")).size(12.0).strong().color(p.text2));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let label = if self.show_pin { tr("Hide") } else { tr("Show") };
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new(label).size(12.0).color(p.text2))
                                            .fill(Color32::TRANSPARENT)
                                            .stroke(Stroke::NONE),
                                    )
                                    .clicked()
                                {
                                    self.show_pin = !self.show_pin;
                                }
                            });
                        });
                        ui.add_space(6.0);
                        digit_boxes(
                            ui,
                            "join_pin",
                            &mut self.viewer_pin,
                            4,
                            !self.show_pin,
                            false,
                        );
                        ui.add_space(14.0);
                        egui::CollapsingHeader::new(RichText::new(tr("Repeater or direct address")).size(13.0).strong())
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.label(RichText::new(tr("Repeater")).size(12.0).color(p.text2));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.viewer_repeater)
                                        .font(egui::FontId::monospace(13.0))
                                        .hint_text("repeater.example.com:5500")
                                        .desired_width(f32::INFINITY),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(tr("Direct address (instead of a code)"))
                                        .size(12.0)
                                        .color(p.text2),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.viewer_direct)
                                        .font(egui::FontId::monospace(13.0))
                                        .hint_text("192.168.1.40:5950")
                                        .desired_width(f32::INFINITY),
                                );
                                ui.checkbox(&mut self.viewer_view_only, tr("View only"));
                            });
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new(fill(tr("{} End-to-end encrypted. The repeater relays the session but cannot read it, and the PIN never leaves this computer."), &[&(char::from(Icon::ShieldCheck))]))
                            .size(12.0)
                            .color(p.text3),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::Button::new(tr("Cancel")).min_size(egui::vec2(0.0, 48.0)))
                                .clicked()
                            {
                                close = true;
                            }
                            let ready = self.build_viewer_connect_request().is_some();
                            let w = ui.available_width();
                            let go = theme::primary(fill(tr("Connect  {}"), &[&(char::from(Icon::ArrowRight))]))
                                .min_size(egui::vec2(w, 48.0));
                            if ui.add_enabled(ready, go).clicked() {
                                request = self.build_viewer_connect_request();
                                if !self.viewer_repeater.trim().is_empty() {
                                    Self::save_pref(
                                        ctx,
                                        KEY_VIEWER_REPEATER,
                                        self.viewer_repeater.trim(),
                                    );
                                }
                                close = true;
                            }
                        });
                    });
            });
        if close {
            self.join_open = false;
        }
        request
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Waiting,
    Active,
    Locked,
}

fn icon_tile(ui: &mut Ui, icon: Icon, size: f32) {
    let p = theme::palette(ui.ctx());
    let (r, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter()
        .rect_filled(r, CornerRadius::same((size * 0.28) as u8), p.accent_soft);
    ui.painter().text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        char::from(icon).to_string(),
        egui::FontId::proportional(size * 0.48),
        p.accent_text,
    );
}

/// Big monospace value on a card (session code or PIN); `refresh` adds a "new value" button.
fn code_card(ui: &mut Ui, title: &str, value: &str, color: Color32, refresh: Option<&mut bool>) {
    let p = theme::palette(ui.ctx());
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(22, 18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // The PIN card has a button in this row; both cards keep the same height.
                ui.set_min_height(30.0);
                ui.label(
                    RichText::new(title.to_uppercase())
                        .size(12.0)
                        .strong()
                        .color(p.text3),
                );
                if let Some(r) = refresh {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(char::from(Icon::RefreshCw).to_string())
                                    .min_size(egui::vec2(30.0, 30.0)),
                            )
                            .on_hover_text(tr("Generate a new code and PIN"))
                            .clicked()
                        {
                            *r = true;
                        }
                    });
                }
            });
            // Largest size that still fits the card on one line.
            let size = ((ui.available_width() - 4.0) / (value.chars().count() as f32 * 0.62))
                .clamp(24.0, 44.0);
            ui.add(
                egui::Label::new(
                    RichText::new(value)
                        .monospace()
                        .size(size)
                        .strong()
                        .color(color),
                )
                .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
}

fn side_card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui.ctx());
    egui::Frame::NONE
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(egui::Margin::same(20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// One row with an icon, a title, a note and a switch. Returns true when the switch was flipped.
fn perm_row(
    ui: &mut Ui,
    icon: Icon,
    title: &str,
    note: &str,
    on: &mut bool,
    enabled: bool,
) -> bool {
    let p = theme::palette(ui.ctx());
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.set_min_height(44.0);
        ui.label(
            RichText::new(char::from(icon).to_string())
                .size(16.0)
                .color(p.text2),
        );
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(14.0));
            ui.label(RichText::new(note).size(12.0).color(p.text3));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (r, resp) = ui.allocate_exact_size(egui::vec2(44.0, 26.0), egui::Sense::click());
            let track = if *on {
                if enabled {
                    theme::ACCENT
                } else {
                    p.border_strong
                }
            } else {
                p.border_strong
            };
            ui.painter().rect_filled(r, CornerRadius::same(13), track);
            let x = if *on {
                r.right() - 13.0
            } else {
                r.left() + 13.0
            };
            ui.painter()
                .circle_filled(egui::pos2(x, r.center().y), 10.0, Color32::WHITE);
            if enabled && resp.clicked() {
                *on = !*on;
                changed = true;
            }
        });
    });
    changed
}

fn safety_card(ui: &mut Ui) {
    let p = theme::palette(ui.ctx());
    side_card(ui, |ui| {
        ui.label(RichText::new(tr("You stay in control")).size(15.0).strong());
        ui.add_space(8.0);
        for (icon, title, body) in [
            (
                Icon::Lock,
                tr("End-to-end encrypted"),
                tr("The repeater only relays bytes it cannot read."),
            ),
            (
                Icon::ShieldCheck,
                tr("Three tries, then locked"),
                tr("A wrong PIN three times revokes the code."),
            ),
            (
                Icon::MousePointer2,
                tr("Your mouse wins"),
                tr("Move yours and remote input pauses for 2 seconds."),
            ),
            (
                Icon::Eye,
                tr("Always visible"),
                tr("A banner stays on top while anyone is connected."),
            ),
        ] {
            ui.horizontal(|ui| {
                icon_tile(ui, icon, 30.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).size(13.0).strong());
                    ui.label(RichText::new(body).size(12.0).color(p.text2));
                });
            });
            ui.add_space(6.0);
        }
    });
}

/// `len` digit boxes fed by one hidden text field, so typing, pasting and backspace all work.
fn digit_boxes(
    ui: &mut Ui,
    salt: &str,
    text: &mut String,
    len: usize,
    mask: bool,
    autofocus: bool,
) {
    let p = theme::palette(ui.ctx());
    text.retain(|c| c.is_ascii_digit());
    text.truncate(len);
    let id = ui.make_persistent_id(salt);
    if autofocus {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let (box_w, gap, split_gap) = (56.0, 8.0, 12.0);
    let extra = if len == 6 { split_gap } else { 0.0 };
    let total = len as f32 * box_w + (len as f32 - 1.0) * gap + extra;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(total, 64.0), egui::Sense::click());
    let mut editor = ui.new_child(
        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(rect.min, egui::vec2(2.0, 2.0))),
    );
    editor.add(
        egui::TextEdit::singleline(text)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(egui::Vec2::ZERO)
            .desired_width(2.0)
            .text_color(Color32::TRANSPARENT)
            .char_limit(len),
    );
    if resp.clicked() {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let focused = ui.memory(|m| m.has_focus(id));
    let cursor = text.chars().count().min(len.saturating_sub(1));
    let digits: Vec<char> = text.chars().collect();
    for i in 0..len {
        let x = rect.left()
            + i as f32 * (box_w + gap)
            + if len == 6 && i >= 3 { split_gap } else { 0.0 };
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(box_w, 64.0));
        let active = focused && i == cursor;
        ui.painter().rect(
            r,
            CornerRadius::same(11),
            p.input_bg,
            Stroke::new(1.5, if active { theme::ACCENT } else { p.border }),
            egui::StrokeKind::Inside,
        );
        if let Some(d) = digits.get(i) {
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                if mask { '•' } else { *d }.to_string(),
                egui::FontId::monospace(28.0),
                p.text,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_listener_host_loop(
    bind_addr: String,
    use_e2ee: bool,
    pin: String,
    config: HostConfig,
    stop_flag: Arc<AtomicBool>,
    active_stream: Arc<Mutex<Option<TcpStream>>>,
    tx: Sender<HostUiEvent>,
    ctx: egui::Context,
) {
    let listener = match TcpListener::bind(&bind_addr) {
        Ok(l) => l,
        Err(e) => {
            let _ = tx.send(HostUiEvent::Stopped(fill(
                tr("Failed to bind Host port {}: {}"),
                &[&bind_addr, &e],
            )));
            ctx.request_repaint();
            return;
        }
    };
    let _ = listener.set_nonblocking(true);
    let _ = tx.send(HostUiEvent::Waiting {
        endpoint: bind_addr.clone(),
        e2ee: use_e2ee,
    });
    ctx.request_repaint();

    let mut limiter = BruteForceLimiter::new();

    while !stop_flag.load(Ordering::SeqCst) && !limiter.is_locked() {
        match listener.accept() {
            Ok((mut stream, peer_addr)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_nodelay(true);
                let peer = peer_addr.to_string();
                if let Ok(cloned) = stream.try_clone() {
                    *active_stream.lock().unwrap() = Some(cloned);
                }

                let res = if use_e2ee {
                    match crate::shared::crypto::host_e2ee_handshake(
                        &mut stream,
                        &pin,
                        &mut limiter,
                    ) {
                        Ok(keys) => {
                            let _ = tx.send(HostUiEvent::Connected {
                                peer: peer.clone(),
                                e2ee: true,
                            });
                            ctx.request_repaint();
                            let mut e2ee =
                                crate::shared::crypto::E2eeStream::new(&mut stream, keys);
                            serve_stream(config.clone(), &mut e2ee)
                        }
                        Err(e) => {
                            let _ = tx.send(HostUiEvent::PinFailed {
                                peer: peer.clone(),
                                attempts: limiter.failures,
                                locked: limiter.is_locked(),
                            });
                            ctx.request_repaint();
                            Err(e)
                        }
                    }
                } else {
                    let _ = tx.send(HostUiEvent::Connected {
                        peer: peer.clone(),
                        e2ee: false,
                    });
                    ctx.request_repaint();
                    serve_stream(config.clone(), &mut stream)
                };

                *active_stream.lock().unwrap() = None;
                if let Err(e) = res {
                    let _ = tx.send(HostUiEvent::Disconnected {
                        peer,
                        reason: e.to_string(),
                    });
                } else {
                    let _ = tx.send(HostUiEvent::Disconnected {
                        peer,
                        reason: tr("Client closed session").into(),
                    });
                }
                if !stop_flag.load(Ordering::SeqCst) && !limiter.is_locked() {
                    let _ = tx.send(HostUiEvent::Waiting {
                        endpoint: bind_addr.clone(),
                        e2ee: use_e2ee,
                    });
                }
                ctx.request_repaint();
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(e) => {
                let _ = tx.send(HostUiEvent::Stopped(fill(tr("Accept error: {}"), &[&e])));
                ctx.request_repaint();
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_repeater_host_loop(
    repeater_addr: String,
    session_code: String,
    pin: String,
    config: HostConfig,
    stop_flag: Arc<AtomicBool>,
    active_stream: Arc<Mutex<Option<TcpStream>>>,
    tx: Sender<HostUiEvent>,
    ctx: egui::Context,
) {
    let mut limiter = BruteForceLimiter::new();
    while !stop_flag.load(Ordering::SeqCst) && !limiter.is_locked() {
        let mut stream = match TcpStream::connect(&repeater_addr) {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(HostUiEvent::Stopped(fill(
                    tr("Failed to reach repeater {}: {}"),
                    &[&repeater_addr, &e],
                )));
                ctx.request_repaint();
                return;
            }
        };
        let _ = stream.set_nodelay(true);
        if let Err(e) = register_repeater_mode_ii(&mut stream, &session_code) {
            let _ = tx.send(HostUiEvent::Stopped(fill(
                tr("Failed to register ID:{} on repeater: {}"),
                &[&session_code, &e],
            )));
            ctx.request_repaint();
            return;
        }
        if let Ok(cloned) = stream.try_clone() {
            *active_stream.lock().unwrap() = Some(cloned);
        }

        let endpoint = format!("{repeater_addr} (ID:{session_code})");
        let _ = tx.send(HostUiEvent::Waiting {
            endpoint: endpoint.clone(),
            e2ee: true,
        });
        ctx.request_repaint();

        match serve_e2ee_stream(config.clone(), &mut stream, &pin, &mut limiter) {
            Ok(()) => {
                *active_stream.lock().unwrap() = None;
                let _ = tx.send(HostUiEvent::Stopped(
                    tr("Repeater E2EE session finished.").into(),
                ));
                ctx.request_repaint();
                return;
            }
            Err(e) => {
                *active_stream.lock().unwrap() = None;
                if limiter.failures > 0 {
                    let _ = tx.send(HostUiEvent::PinFailed {
                        peer: endpoint,
                        attempts: limiter.failures,
                        locked: limiter.is_locked(),
                    });
                } else {
                    let _ = tx.send(HostUiEvent::Stopped(fill(tr("Session closed: {}"), &[&e])));
                }
                ctx.request_repaint();
                if stop_flag.load(Ordering::SeqCst) || limiter.is_locked() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_invitation_in_english_and_in_spanish() {
        let en = "Lucarna QuickSupport Invitation\nRepeater: 203.0.113.10:5500\nSession Code: 842-786 (ID:842786)\nE2EE PIN: 4138";
        let inv = parse_invitation(en).unwrap();
        assert_eq!(inv.code.as_deref(), Some("842786"));
        assert_eq!(inv.pin.as_deref(), Some("4138"));
        assert_eq!(inv.repeater.as_deref(), Some("203.0.113.10:5500"));

        let es = "Invitación de Lucarna Studio E2EE\nDirección: 127.0.0.1:5950\nCódigo de sesión: 367-842\nPIN E2EE: 2329";
        let inv = parse_invitation(es).unwrap();
        assert_eq!(inv.address.as_deref(), Some("127.0.0.1:5950"));
        assert_eq!(inv.code.as_deref(), Some("367842"));
        assert_eq!(inv.pin.as_deref(), Some("2329"));
    }

    #[test]
    fn plain_digits_are_not_an_invitation() {
        assert_eq!(parse_invitation("582914"), None);
        assert_eq!(parse_invitation("Session Code: 842-786"), None);
    }

    #[test]
    fn viewer_connect_card_maps_numeric_code_with_repeater_to_id_format() {
        let view = HostView {
            viewer_target_code: "582-914".into(),
            viewer_pin: "4920".into(),
            viewer_repeater: "127.0.0.1:5901".into(),
            ..Default::default()
        };

        let req = view.build_viewer_connect_request().unwrap();
        assert_eq!(req.address.as_str(), "ID:582914");
        assert_eq!(req.repeater.unwrap().as_str(), "127.0.0.1:5901");
        assert_eq!(req.creds.pin, "4920");
    }

    #[test]
    fn regenerate_credentials_resets_lockout_and_records_audit_log() {
        let mut view = HostView {
            status: HostStatus::Locked {
                reason: "3 failed attempts".into(),
            },
            ..Default::default()
        };
        let old_code = view.session_code.clone();
        view.regenerate_credentials();
        assert_eq!(view.status, HostStatus::Idle);
        assert_eq!(view.session_code.len(), 6);
        assert_eq!(view.pin.len(), 4);
        assert!(!view.audit_log.is_empty());
        let _ = old_code;
    }
}
