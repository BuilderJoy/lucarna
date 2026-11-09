//! Studio Host & QuickSupport UI (`src/features/host/view.rs`):
//! - "Share this computer" control center (6-digit session code + 4-digit E2EE PIN generator)
//! - QuickSupport E2EE Viewer connection card (connect via Code + PIN + Repeater Mode II)
//! - Permanent unhideable active-session security banner with instant red "Terminate Now" button
//! - Real-time anti-RAT audit log and virtual drive (`M:\`, `D:\`, `H:\`, `V:\`) status inspector

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
            stop_flag: Arc::new(AtomicBool::new(false)),
            active_stream: Arc::new(Mutex::new(None)),
            events_rx: None,
        }
    }
}

impl HostView {
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
            format!(
                "Generated new session code {} and 4-digit E2EE PIN.",
                format_code(&self.session_code)
            ),
            false,
        );
    }

    pub fn format_invitation(&self) -> String {
        let code_fmt = format_code(&self.session_code);
        if self.use_repeater {
            format!(
                "Lucarna QuickSupport Invitation\nRepeater: {}\nSession Code: {} (ID:{})\nE2EE PIN: {}",
                self.repeater_addr.trim(),
                code_fmt,
                normalize_code(&self.session_code),
                self.pin
            )
        } else if self.use_e2ee_pin {
            format!(
                "Lucarna Studio E2EE Invitation\nAddress: {}\nSession Code: {}\nE2EE PIN: {}",
                self.bind_addr.trim(),
                code_fmt,
                self.pin
            )
        } else {
            format!(
                "Lucarna Studio Host\nAddress: {}\nPassword: {}",
                self.bind_addr.trim(),
                self.studio_password
            )
        }
    }

    /// Builds a `ConnectRequest` from the QuickSupport operator card.
    pub fn build_viewer_connect_request(&self) -> Option<ConnectRequest> {
        let raw_target = self.viewer_target_code.trim();
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
        self.stop_flag = Arc::new(AtomicBool::new(false));
        self.active_stream = Arc::new(Mutex::new(None));

        let (tx, rx) = mpsc::channel();
        self.events_rx = Some(rx);

        let stop_flag = self.stop_flag.clone();
        let active_stream = self.active_stream.clone();
        let use_repeater = self.use_repeater;
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
                desktop_name: "Lucarna Studio Host".into(),
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
                        format!(
                            "Waiting for remote connection on {endpoint} ({}).",
                            if e2ee {
                                "E2EE ChaCha20-Poly1305"
                            } else {
                                "RFB 3.8"
                            }
                        ),
                        false,
                    );
                }
                HostUiEvent::Connected { peer, e2ee } => {
                    self.status = HostStatus::Active {
                        peer: peer.clone(),
                        e2ee,
                    };
                    self.push_audit(
                        format!(
                            "Session established with {peer} ({}).",
                            if e2ee {
                                "Verified E2EE PIN"
                            } else {
                                "VNC Auth"
                            }
                        ),
                        false,
                    );
                }
                HostUiEvent::Disconnected { peer, reason } => {
                    self.push_audit(format!("Session ended with {peer}: {reason}"), false);
                }
                HostUiEvent::PinFailed {
                    peer,
                    attempts,
                    locked,
                } => {
                    self.push_audit(
                        format!("SECURITY ALERT: Failed E2EE PIN attempt #{attempts} from {peer}."),
                        true,
                    );
                    if locked {
                        let reason = format!(
                            "Locked after {attempts} failed PIN attempts from {peer}. Regenerate code to resume."
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

    /// Permanent, unhideable floating anti-RAT banner drawn whenever this computer is being shared.
    pub fn show_active_banner(&mut self, ctx: &egui::Context) {
        self.poll_events();
        if !self.is_running() {
            return;
        }

        let (is_active, headline, subline) = match &self.status {
            HostStatus::Active { peer, e2ee } => (
                true,
                format!("ACTIVE ASSISTANCE SESSION WITH {}", peer.to_uppercase()),
                if *e2ee {
                    "E2EE ChaCha20-Poly1305 Active · Local Mouse Priority (2s cooldown) Enabled"
                } else {
                    "RFB 3.8 Session Active · Local Mouse Priority (2s cooldown) Enabled"
                },
            ),
            HostStatus::Waiting { endpoint, e2ee } => (
                false,
                format!("SHARING THIS COMPUTER — WAITING ON {endpoint}"),
                if *e2ee {
                    "Protected by 4-digit E2EE PIN (3-attempt brute-force lockout)"
                } else {
                    "Waiting for studio operator connection"
                },
            ),
            _ => return,
        };

        let bg_fill = if is_active {
            Color32::from_rgb(0x45, 0x12, 0x16)
        } else {
            Color32::from_rgb(0x2d, 0x22, 0x0e)
        };
        let border_color = if is_active {
            Color32::from_rgb(0xff, 0x4d, 0x4d)
        } else {
            Color32::from_rgb(0xff, 0xbe, 0x3d)
        };

        egui::Area::new(egui::Id::unique("anti_rat_host_banner"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -38.0))
            .order(Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::window(ui.style())
                    .fill(bg_fill)
                    .stroke(Stroke::new(2.0, border_color))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{}  {headline}",
                                    char::from(Icon::ShieldAlert)
                                ))
                                .color(Color32::WHITE)
                                .strong()
                                .size(13.5),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(subline)
                                    .color(Color32::from_rgb(230, 215, 200))
                                    .size(11.5),
                            );
                            ui.add_space(16.0);
                            let stop_btn = egui::Button::new(
                                RichText::new(format!(
                                    "{} Terminate Session Now",
                                    char::from(Icon::OctagonX)
                                ))
                                .color(Color32::WHITE)
                                .strong(),
                            )
                            .fill(Color32::from_rgb(0xd9, 0x26, 0x26));
                            if ui
                                .add(stop_btn)
                                .on_hover_text(
                                    "Immediately disconnect remote operator and stop sharing",
                                )
                                .clicked()
                            {
                                self.stop_sharing();
                            }
                        });
                    });
            });
    }

    /// Main canvas view for the "Share & QuickSupport" dashboard tab.
    pub fn show_panel(&mut self, ui: &mut Ui) -> Option<ConnectRequest> {
        self.poll_events();
        let mut connect_request = None;
        let accent = ui.visuals().selection.bg_fill;

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{}  Studio Host & QuickSupport (E2EE)",
                    char::from(Icon::Radio)
                ))
                .size(18.0)
                .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (badge_text, badge_color) = match &self.status {
                    HostStatus::Idle => ("● Idle (Not sharing)", Color32::GRAY),
                    HostStatus::Waiting { .. } => (
                        "● Waiting for connection…",
                        Color32::from_rgb(0xff, 0xbe, 0x3d),
                    ),
                    HostStatus::Active { .. } => {
                        ("● LIVE SESSION ACTIVE", Color32::from_rgb(80, 220, 120))
                    }
                    HostStatus::Locked { .. } => (
                        "● LOCKED (3 Failed PIN Attempts)",
                        Color32::from_rgb(255, 80, 80),
                    ),
                };
                ui.label(RichText::new(badge_text).color(badge_color).strong());
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Share this computer in user space (no root/SYSTEM service) with native file transfer and ChaCha20-Poly1305 end-to-end encryption, or join a remote studio.",
            )
            .weak()
            .size(12.5),
        );
        ui.add_space(14.0);

        egui::ScrollArea::vertical()
            .id_salt("host_quicksupport_scroll")
            .show(ui, |ui| {
                ui.columns(2, |cols| {
                    // LEFT COLUMN: SHARE THIS COMPUTER (HOST)
                    let left = &mut cols[0];
                    egui::Frame::group(left.style())
                        .fill(Color32::from_rgb(0x14, 0x1c, 0x2b))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(0x28, 0x36, 0x50)))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(16))
                        .show(left, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{}  Share This Computer (Host)",
                                        char::from(Icon::MonitorUp)
                                    ))
                                    .size(15.5)
                                    .strong()
                                    .color(accent),
                                );
                            });
                            ui.add_space(8.0);

                            // Sharing credentials card (Code + PIN)
                            egui::Frame::NONE
                                .fill(Color32::from_rgb(0x0c, 0x11, 0x1b))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(0x24, 0x32, 0x4a)))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(egui::Margin::same(12))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.label(
                                                RichText::new("SESSION CODE")
                                                    .size(10.5)
                                                    .weak()
                                                    .strong(),
                                            );
                                            ui.label(
                                                RichText::new(format_code(&self.session_code))
                                                    .size(24.0)
                                                    .monospace()
                                                    .strong()
                                                    .color(Color32::WHITE),
                                            );
                                        });
                                        ui.add_space(24.0);
                                        ui.vertical(|ui| {
                                            ui.label(
                                                RichText::new("ONE-TIME E2EE PIN")
                                                    .size(10.5)
                                                    .weak()
                                                    .strong(),
                                            );
                                            ui.label(
                                                RichText::new(&self.pin)
                                                    .size(24.0)
                                                    .monospace()
                                                    .strong()
                                                    .color(accent),
                                            );
                                        });
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                let copied = self
                                                    .copied_until
                                                    .is_some_and(|t| Instant::now() < t);
                                                let copy_label = if copied {
                                                    format!("{} Copied!", char::from(Icon::Check))
                                                } else {
                                                    format!("{} Copy Invite", char::from(Icon::Copy))
                                                };
                                                if ui
                                                    .button(copy_label)
                                                    .on_hover_text(
                                                        "Copy connection code and PIN to clipboard",
                                                    )
                                                    .clicked()
                                                {
                                                    ui.ctx().copy_text(self.format_invitation());
                                                    self.copied_until = Some(
                                                        Instant::now() + Duration::from_secs(2),
                                                    );
                                                }
                                                if ui
                                                    .add_enabled(
                                                        !self.is_running(),
                                                        egui::Button::new(format!(
                                                            "{}",
                                                            char::from(Icon::RefreshCw)
                                                        )),
                                                    )
                                                    .on_hover_text(
                                                        "Generate a new random Session Code & PIN",
                                                    )
                                                    .clicked()
                                                {
                                                    self.regenerate_credentials();
                                                }
                                            },
                                        );
                                    });
                                });

                            ui.add_space(10.0);

                            let running = self.is_running();
                            ui.add_enabled_ui(!running, |ui| {
                                ui.horizontal(|ui| {
                                    ui.selectable_value(
                                        &mut self.use_repeater,
                                        false,
                                        format!("{} Direct Port (LAN)", char::from(Icon::Network)),
                                    );
                                    ui.selectable_value(
                                        &mut self.use_repeater,
                                        true,
                                        format!(
                                            "{} Repeater Mode II (NAT)",
                                            char::from(Icon::Globe)
                                        ),
                                    );
                                });
                                ui.add_space(6.0);

                                if self.use_repeater {
                                    ui.label(RichText::new("Repeater Mode II Server").size(12.0));
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.repeater_addr)
                                            .hint_text("repeater.example.com:5500")
                                            .desired_width(f32::INFINITY),
                                    );
                                } else {
                                    ui.label(RichText::new("Listen Address (Host:Port)").size(12.0));
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.bind_addr)
                                            .hint_text("0.0.0.0:5950")
                                            .desired_width(f32::INFINITY),
                                    );
                                    ui.add_space(4.0);
                                    ui.checkbox(
                                        &mut self.use_e2ee_pin,
                                        "Require E2EE tunnel & 4-digit PIN (ChaCha20-Poly1305)",
                                    );
                                    if !self.use_e2ee_pin {
                                        ui.label(
                                            RichText::new("Classic VNC Password").size(12.0),
                                        );
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.studio_password)
                                                .password(true)
                                                .desired_width(f32::INFINITY),
                                        );
                                    }
                                }

                                ui.add_space(4.0);
                                ui.checkbox(
                                    &mut self.view_only,
                                    "View-only mode (block remote mouse, keyboard & file uploads)",
                                );
                            });

                            ui.add_space(12.0);
                            if !running {
                                let start_btn = egui::Button::new(
                                    RichText::new(format!(
                                        "{}  Start Sharing This Computer",
                                        char::from(Icon::Play)
                                    ))
                                    .color(Color32::BLACK)
                                    .strong(),
                                )
                                .fill(accent)
                                .min_size(egui::vec2(ui.available_width(), 34.0));
                                if ui.add(start_btn).clicked() {
                                    let ctx = ui.ctx().clone();
                                    self.start_sharing(&ctx);
                                }
                            } else {
                                let stop_btn = egui::Button::new(
                                    RichText::new(format!(
                                        "{}  Terminate Sharing Now",
                                        char::from(Icon::OctagonX)
                                    ))
                                    .color(Color32::WHITE)
                                    .strong(),
                                )
                                .fill(Color32::from_rgb(0xd9, 0x26, 0x26))
                                .min_size(egui::vec2(ui.available_width(), 34.0));
                                if ui.add(stop_btn).clicked() {
                                    self.stop_sharing();
                                }
                            }
                        });

                    // RIGHT COLUMN: CONNECT TO REMOTE QUICKSUPPORT HOST (VIEWER)
                    let right = &mut cols[1];
                    egui::Frame::group(right.style())
                        .fill(Color32::from_rgb(0x14, 0x1c, 0x2b))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(0x28, 0x36, 0x50)))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(16))
                        .show(right, |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{}  Assist a Remote Computer (E2EE Viewer)",
                                    char::from(Icon::ShieldCheck)
                                ))
                                .size(15.5)
                                .strong()
                                .color(accent),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "Enter the Session Code/Address and 4-digit PIN shared by the remote studio or broadcaster.",
                                )
                                .size(12.0)
                                .weak(),
                            );
                            ui.add_space(10.0);

                            ui.label(
                                RichText::new("Remote Address or 6-Digit Session Code").size(12.0),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut self.viewer_target_code)
                                    .hint_text("e.g. 127.0.0.1:5950 or 582-914")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add_space(6.0);

                            ui.label(RichText::new("4-Digit E2EE PIN").size(12.0));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.viewer_pin)
                                    .hint_text("e.g. 4920 (leave empty for unencrypted VNC)")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add_space(6.0);

                            ui.label(
                                RichText::new("Repeater Mode II Server (optional)").size(12.0),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut self.viewer_repeater)
                                    .hint_text("e.g. repeater.example.com:5901")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add_space(6.0);

                            ui.checkbox(
                                &mut self.viewer_view_only,
                                "Connect in view-only mode (observe without interfering)",
                            );

                            ui.add_space(12.0);
                            let can_connect = self.build_viewer_connect_request().is_some();
                            let join_btn = egui::Button::new(
                                RichText::new(format!(
                                    "{}  Connect with E2EE",
                                    char::from(Icon::Lock)
                                ))
                                .color(Color32::BLACK)
                                .strong(),
                            )
                            .fill(accent)
                            .min_size(egui::vec2(ui.available_width(), 34.0));
                            if ui.add_enabled(can_connect, join_btn).clicked() {
                                connect_request = self.build_viewer_connect_request();
                            }
                        });
                });

                ui.add_space(16.0);

                // BOTTOM SECTION: SANDBOXED VIRTUAL DRIVES & REAL-TIME SECURITY AUDIT LOG
                ui.columns(2, |cols| {
                    let left = &mut cols[0];
                    egui::Frame::group(left.style())
                        .fill(Color32::from_rgb(0x12, 0x19, 0x26))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(0x22, 0x2e, 0x44)))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(14))
                        .show(left, |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{}  Sandboxed Virtual Drives (Native RFB Msg 7 — No SSH Needed)",
                                    char::from(Icon::HardDrive)
                                ))
                                .strong()
                                .size(13.5),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "Protected by std::fs::canonicalize: blocks '../' traversal, dotfiles (.ssh, .env) and external symlinks.",
                                )
                                .size(11.5)
                                .weak(),
                            );
                            ui.add_space(8.0);
                            for (letter, path) in self.drives.roots() {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("{letter}:\\"))
                                            .monospace()
                                            .strong()
                                            .color(accent),
                                    );
                                    ui.label(
                                        RichText::new(path.display().to_string())
                                            .monospace()
                                            .size(12.0),
                                    );
                                });
                            }
                        });

                    let right = &mut cols[1];
                    egui::Frame::group(right.style())
                        .fill(Color32::from_rgb(0x12, 0x19, 0x26))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(0x22, 0x2e, 0x44)))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(14))
                        .show(right, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{}  Anti-RAT Security & Session Audit Log",
                                        char::from(Icon::Shield)
                                    ))
                                    .strong()
                                    .size(13.5),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if !self.audit_log.is_empty()
                                        && ui.small_button("Clear").clicked()
                                    {
                                        self.audit_log.clear();
                                    }
                                });
                            });
                            ui.add_space(6.0);
                            if self.audit_log.is_empty() {
                                ui.label(
                                    RichText::new(
                                        "No session events yet. Connections, PIN verifications and lockouts appear here in real time.",
                                    )
                                    .size(12.0)
                                    .weak(),
                                );
                            } else {
                                for entry in self.audit_log.iter().take(8) {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(&entry.timestamp)
                                                .monospace()
                                                .size(11.5)
                                                .weak(),
                                        );
                                        let color = if entry.is_alert {
                                            Color32::from_rgb(255, 100, 100)
                                        } else {
                                            ui.visuals().text_color()
                                        };
                                        ui.label(
                                            RichText::new(&entry.message).size(12.0).color(color),
                                        );
                                    });
                                }
                            }
                        });
                });
            });

        connect_request
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
            let _ = tx.send(HostUiEvent::Stopped(format!(
                "Failed to bind Host port {bind_addr}: {e}"
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
                        reason: "Client closed session".into(),
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
                let _ = tx.send(HostUiEvent::Stopped(format!("Accept error: {e}")));
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
                let _ = tx.send(HostUiEvent::Stopped(format!(
                    "Failed to reach repeater {repeater_addr}: {e}"
                )));
                ctx.request_repaint();
                return;
            }
        };
        let _ = stream.set_nodelay(true);
        if let Err(e) = register_repeater_mode_ii(&mut stream, &session_code) {
            let _ = tx.send(HostUiEvent::Stopped(format!(
                "Failed to register ID:{session_code} on repeater: {e}"
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
                    "Repeater E2EE session finished.".into(),
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
                    let _ = tx.send(HostUiEvent::Stopped(format!("Session closed: {e}")));
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
