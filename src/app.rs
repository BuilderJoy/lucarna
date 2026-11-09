//! App shell: switches between the dashboard and a live session.

use crate::shared::i18n::{fill, tr};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Layout, RichText};

use crate::features::chat::{ChatView, Msg};
use crate::features::clipboard::{ClipboardView, RemoteClipboard};
use crate::features::connection::{
    ConnectRequest, Dashboard, ServerAddress, SessionOptions, SshTarget,
};
use crate::features::file_transfer::{DualFiles, FilesView};
use crate::features::input::{self, InputForwarder};
use crate::features::remote_screen::{self, Framebuffer, Scale, ScreenView};
use crate::session::{self, Event, Outbound};
use crate::shared::theme;

const PREVIEW_INTERVAL: Duration = Duration::from_secs(5);
const BELL_FLASH: Duration = Duration::from_secs(2);

struct LiveSession {
    address: ServerAddress,
    name: String,
    outbound: Arc<Outbound>,
    screen: Arc<Mutex<Framebuffer>>,
    view: ScreenView,
    input: InputForwarder,
    files: Arc<DualFiles>,
    files_view: FilesView,
    chat: ChatView,
    clipboard: Arc<RemoteClipboard>,
    clipboard_view: ClipboardView,
    /// Don't forward keyboard, mouse or clipboard to the server.
    view_only: bool,
    share_clipboard: bool,
    save_preview: bool,
    last_preview: Option<Instant>,
    bell_until: Option<Instant>,
    user_disconnected: bool,
    scale: Scale,
    /// Where the session bar floats in full screen, so clicks on it don't reach the remote machine.
    bar_rect: Option<egui::Rect>,
    /// False until the first FramebufferUpdate lands; a full Raw frame can take seconds on slow links.
    has_frame: bool,
    /// How the session is protected, shown as a badge: an SSH tunnel or the end-to-end PIN tunnel.
    secure: Option<&'static str>,
    fullscreen_on_open: bool,
    /// Width the centred tool group took last frame, to centre it this frame.
    tools_w: f32,
}

/// One open connection: still connecting (`live` is `None`) or running.
struct Tab {
    events: mpsc::Receiver<Event>,
    pending: Option<(ServerAddress, SessionOptions)>,
    secure: Option<&'static str>,
    /// What the tab shows until the server tells us its name.
    title: String,
    live: Option<LiveSession>,
}

enum Outcome {
    Nothing,
    Connected,
    Closed(String),
}

#[derive(Default)]
pub struct App {
    dashboard: Dashboard,
    tabs: Vec<Tab>,
    /// Index into `tabs`; `None` shows the Address Book.
    active: Option<usize>,
}

impl App {
    fn connect(&mut self, ctx: &egui::Context, req: ConnectRequest) {
        // A bad SSH field is an error, never a silent fallback to an unencrypted connection.
        let ssh = match SshTarget::parse(&req.ssh) {
            Ok(ssh) => ssh,
            Err(e) => {
                self.dashboard.status = e;
                return;
            }
        };
        let secure = if ssh.is_some() {
            Some(tr("SSH encrypted"))
        } else if !req.creds.pin.is_empty() {
            Some(tr("End-to-end encrypted"))
        } else {
            None
        };
        self.dashboard.last_request = Some(req.clone());
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let (address, repeater, creds) = (req.address.clone(), req.repeater, req.creds);
        self.tabs.push(Tab {
            events: rx,
            pending: Some((req.address.clone(), req.options)),
            secure,
            title: req.address.as_str().to_string(),
            live: None,
        });
        self.active = Some(self.tabs.len() - 1);
        std::thread::spawn(move || {
            session::run(
                &address,
                repeater.as_ref(),
                ssh.as_ref(),
                &creds,
                tx,
                move || ctx.request_repaint(),
            )
        });
        self.dashboard.status.clear();
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        let mut closed = Vec::new();
        for i in 0..self.tabs.len() {
            let is_active = self.active == Some(i);
            while let Ok(event) = self.tabs[i].events.try_recv() {
                match handle_event(
                    &mut self.dashboard,
                    &mut self.tabs[i],
                    ctx,
                    event,
                    is_active,
                ) {
                    Outcome::Nothing => {}
                    Outcome::Connected => {
                        // The user asked for this connection from the Address Book (or is waiting on it).
                        if self.active.is_none() || is_active {
                            self.active = Some(i);
                            if self.tabs[i]
                                .live
                                .as_ref()
                                .is_some_and(|l| l.fullscreen_on_open)
                            {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                            }
                        }
                    }
                    Outcome::Closed(reason) => {
                        closed.push((i, reason));
                        break;
                    }
                }
            }
        }
        for (i, reason) in closed.into_iter().rev() {
            // A deliberate disconnect has an empty reason and clears the message.
            if self.active == Some(i) || !reason.is_empty() {
                self.dashboard.status = reason;
            }
            self.remove_tab(i);
        }
    }

    fn remove_tab(&mut self, i: usize) {
        self.tabs.remove(i);
        self.active = match self.active {
            Some(a) if a == i => None,
            Some(a) if a > i => Some(a - 1),
            other => other,
        };
    }

    /// Browser-style strip: the Address Book, one tab per connection, and a new-connection button.
    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        use lucide_icons::Icon;
        let p = theme::palette(ui.ctx());
        let (mut select, mut close) = (None, None);
        let frame = egui::Frame::NONE
            .fill(p.bg)
            .inner_margin(egui::Margin::symmetric(12, 6));
        egui::Panel::top("session_tabs")
            .frame(frame)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let home = self.active.is_none();
                    let btn = egui::Button::new(char::from(Icon::LayoutGrid).to_string())
                        .fill(if home {
                            p.elevated
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(egui::Stroke::NONE)
                        .min_size(egui::vec2(36.0, 32.0));
                    if ui.add(btn).on_hover_text(tr("Address book")).clicked() {
                        select = Some(None);
                    }
                    for (i, t) in self.tabs.iter().enumerate() {
                        let sel = self.active == Some(i);
                        let (name, up) = match &t.live {
                            Some(l) => (l.name.as_str(), l.has_frame),
                            None => (t.title.as_str(), false),
                        };
                        egui::Frame::NONE
                            .fill(if sel { p.surface } else { Color32::TRANSPARENT })
                            .stroke(egui::Stroke::new(
                                1.0,
                                if sel { p.border } else { Color32::TRANSPARENT },
                            ))
                            .corner_radius(egui::CornerRadius {
                                nw: 9,
                                ne: 9,
                                sw: 0,
                                se: 0,
                            })
                            .inner_margin(egui::Margin::symmetric(12, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if t.live.is_none() {
                                        ui.add(egui::Spinner::new().size(12.0));
                                    } else {
                                        let dot = if up {
                                            p.success
                                        } else {
                                            ui.visuals().warn_fg_color
                                        };
                                        ui.label(RichText::new("●").size(10.0).color(dot));
                                    }
                                    let label = egui::Label::new(
                                        RichText::new(name)
                                            .color(if sel { p.text } else { p.text2 })
                                            .strong(),
                                    )
                                    .sense(egui::Sense::click());
                                    if ui.add(label).clicked() {
                                        select = Some(Some(i));
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(char::from(Icon::X).to_string())
                                                .small()
                                                .frame(false),
                                        )
                                        .on_hover_text(tr("Close session"))
                                        .clicked()
                                    {
                                        close = Some(i);
                                    }
                                });
                            });
                    }
                    if ui
                        .add(
                            egui::Button::new(char::from(Icon::Plus).to_string())
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE)
                                .min_size(egui::vec2(30.0, 30.0)),
                        )
                        .on_hover_text(tr("New connection"))
                        .clicked()
                    {
                        select = Some(None);
                    }
                });
            });
        if let Some(s) = select {
            self.active = s;
        }
        if let Some(i) = close {
            match self.tabs[i].live.as_mut() {
                // The session thread answers with `Closed`, which removes the tab.
                Some(l) => {
                    l.user_disconnected = true;
                    l.outbound.close();
                }
                None => self.remove_tab(i),
            }
        }
    }
}

/// Applies one session event to its tab.
fn handle_event(
    dashboard: &mut Dashboard,
    tab: &mut Tab,
    ctx: &egui::Context,
    event: Event,
    is_active: bool,
) -> Outcome {
    match event {
        Event::Connected {
            name,
            outbound,
            screen,
            files,
            clipboard,
        } => {
            dashboard.status.clear();
            let (address, opts) = tab.pending.take().unwrap_or_else(|| {
                (
                    ServerAddress::parse("127.0.0.1").unwrap(),
                    SessionOptions::default(),
                )
            });
            dashboard.mark_connected(&address);
            let scale = if opts.scale_1_1 {
                Scale::Actual
            } else {
                Scale::Fit
            };
            tab.live = Some(LiveSession {
                address,
                name,
                outbound,
                screen,
                view: ScreenView::default(),
                input: InputForwarder::default(),
                files,
                files_view: FilesView::default(),
                chat: ChatView::default(),
                clipboard,
                clipboard_view: ClipboardView::default(),
                view_only: opts.view_only,
                share_clipboard: opts.share_clipboard,
                save_preview: opts.save_preview,
                last_preview: None,
                bell_until: None,
                user_disconnected: false,
                scale,
                bar_rect: None,
                has_frame: false,
                secure: tab.secure,
                tools_w: 0.0,
                fullscreen_on_open: opts.fullscreen,
            });
            Outcome::Connected
        }
        Event::Bell => {
            if let Some(l) = &mut tab.live {
                l.bell_until = Some(Instant::now() + BELL_FLASH);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
            ctx.request_repaint_after(BELL_FLASH);
            Outcome::Nothing
        }
        Event::Chat(msg) => {
            if matches!(msg, Msg::Text(_)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                    egui::UserAttentionType::Informational,
                ));
            }
            if let Some(l) = &mut tab.live {
                l.chat.apply(msg);
            }
            Outcome::Nothing
        }
        Event::Files(update) => {
            if let Some(l) = &mut tab.live {
                if matches!(
                    update,
                    crate::features::file_transfer::Update::Access(false)
                ) && l.files.has_sftp()
                    && !l.files.is_sftp_active()
                {
                    l.files.switch_to_sftp();
                } else {
                    l.files_view.apply(update, &*l.files);
                }
            }
            Outcome::Nothing
        }
        Event::ScreenUpdated => {
            if let Some(l) = &mut tab.live {
                l.has_frame = true;
                if l.save_preview
                    && l.last_preview
                        .is_none_or(|t| t.elapsed() >= PREVIEW_INTERVAL)
                {
                    let (w, h, rgba) = l.screen.lock().unwrap().thumbnail(320, 200);
                    dashboard.update_preview(ctx, &l.address, w, h, &rgba);
                    l.last_preview = Some(Instant::now());
                }
            }
            Outcome::Nothing
        }
        Event::ServerClipboard(text) => {
            if let Some(l) = tab.live.as_mut()
                && l.share_clipboard
                && !l.view_only
            {
                ctx.copy_text(text.clone());
                l.clipboard_view.push(text, true);
            }
            Outcome::Nothing
        }
        Event::Closed(reason) => {
            let intentional = tab.live.as_ref().is_some_and(|l| l.user_disconnected);
            if let Some(l) = tab.live.take()
                && l.has_frame
                && l.save_preview
            {
                let (w, h, rgba) = l.screen.lock().unwrap().thumbnail(320, 200);
                dashboard.update_preview(ctx, &l.address, w, h, &rgba);
            }
            if is_active && ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
            Outcome::Closed(if intentional { String::new() } else { reason })
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_events(&ctx);
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if !fullscreen && !self.tabs.is_empty() {
            self.tab_strip(ui);
        }
        let live_idx = self.active.filter(|&i| self.tabs[i].live.is_some());
        let Some(live) = live_idx.and_then(|i| self.tabs[i].live.as_mut()) else {
            // Active but not live means a connection is still being made.
            let busy = self.active.is_some();
            if let Some(req) = self.dashboard.show(ui, busy) {
                self.connect(&ctx, req);
            }
            self.dashboard.host_view.show_active_banner(&ctx);
            return;
        };
        // Round-trip measurement for the bar; the answer arrives as a normal update.
        if live.has_frame && live.outbound.stats().ping_due() {
            let _ = remote_screen::ping(&live.outbound);
        }
        ctx.request_repaint_after(Duration::from_secs(2));
        // Our own shortcut: consumed before input forwarding so the remote machine never sees it.
        let toggle = ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::F,
            )
        });
        if toggle {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
        let bar =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(14, 8));
        if !fullscreen {
            live.bar_rect = None;
            egui::Panel::top("bar")
                .frame(bar)
                .show(ui, |ui| bar_contents(ui, live, fullscreen));
        } else {
            // In full screen the bar floats over the screen and only appears when the pointer reaches the top.
            let near_top = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| p.y < 48.0));
            let bell_active = live.bell_until.is_some_and(|t| Instant::now() < t);
            let show = near_top || ctx.any_popup_open() || !live.has_frame || bell_active;
            live.bar_rect = show.then(|| {
                egui::Area::new(egui::Id::unique("fullscreen_bar"))
                    .anchor(egui::Align2::LEFT_TOP, egui::Vec2::ZERO)
                    .order(egui::Order::Foreground)
                    .show(&ctx, |ui| {
                        bar.show(ui, |ui| {
                            ui.set_width(ctx.content_rect().width() - 24.0);
                            bar_contents(ui, live, fullscreen);
                        });
                    })
                    .response
                    .rect
            });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| {
                if !live.has_frame {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() / 2.0 - 40.0);
                        ui.add(egui::Spinner::new().size(32.0));
                        ui.add_space(8.0);
                        ui.label(RichText::new(tr("Waiting for the remote screen…")).weak());
                    });
                    return;
                }
                let viewport = live.view.show(ui, &live.screen, live.scale);
                // Clicks on our own windows and typing in our text inputs must not reach the remote machine.
                let over_ui = [
                    live.files_view.rect,
                    live.chat.rect,
                    live.clipboard_view.rect,
                    live.bar_rect,
                ]
                .into_iter()
                .flatten()
                .any(|r| ui.input(|i| i.pointer.hover_pos().is_some_and(|p| r.contains(p))));
                live.view.draw_cursor(ui, &viewport, !over_ui);
                if !over_ui && !live.view_only && !ui.ctx().egui_wants_keyboard_input() {
                    if live.share_clipboard
                        && let Some(text) = live.clipboard.forward_paste(ui)
                    {
                        live.clipboard_view.push(text, false);
                    }
                    live.input.forward(ui, &viewport, &live.outbound);
                }
            });
        live.files_view.show(&ctx, &*live.files);
        live.chat.show(&ctx, &live.outbound);
        live.clipboard_view.show(
            &ctx,
            &live.clipboard,
            live.share_clipboard && !live.view_only,
        );
        self.dashboard.host_view.show_active_banner(&ctx);
    }
}

/// One toolbar button; `on` paints it as a toggled-on amber chip. Icon only when `compact`.
fn tool(
    ui: &mut egui::Ui,
    icon: lucide_icons::Icon,
    label: &str,
    on: bool,
    compact: bool,
    tip: &str,
) -> bool {
    let p = theme::palette(ui.ctx());
    let (fg, fill, stroke) = if on {
        (
            p.accent_text,
            p.accent_soft,
            egui::Stroke::new(1.0, theme::ACCENT),
        )
    } else {
        (p.text2, Color32::TRANSPARENT, egui::Stroke::NONE)
    };
    let text = if compact {
        RichText::new(char::from(icon).to_string())
    } else {
        RichText::new(format!("{} {label}", char::from(icon)))
    };
    ui.add(
        egui::Button::new(text.color(fg))
            .fill(fill)
            .stroke(stroke)
            .min_size(egui::vec2(0.0, 34.0)),
    )
    .on_hover_text(if compact {
        format!("{label}: {tip}")
    } else {
        tip.to_string()
    })
    .clicked()
}

/// Left: encryption and resolution. Centre: view and remote tools. Right: Disconnect.
fn bar_contents(ui: &mut egui::Ui, live: &mut LiveSession, fullscreen: bool) {
    use lucide_icons::Icon;
    let p = theme::palette(ui.ctx());
    const H: f32 = 38.0;
    const LEFT_W: f32 = 340.0;
    const RIGHT_W: f32 = 140.0;
    let center_w =
        (ui.available_width() - LEFT_W - RIGHT_W - 2.0 * ui.spacing().item_spacing.x).max(0.0);
    let compact = center_w < 800.0;
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(LEFT_W, H),
            Layout::left_to_right(Align::Center),
            |ui| {
                let dot = if live.has_frame {
                    p.success
                } else {
                    ui.visuals().warn_fg_color
                };
                ui.label(RichText::new("●").size(10.0).color(dot));
                if let Some(label) = live.secure {
                    egui::Frame::NONE
                        .fill(p.success.gamma_multiply(0.16))
                        .corner_radius(egui::CornerRadius::same(14))
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("{} {label}", char::from(Icon::Lock)))
                                    .size(12.0)
                                    .strong()
                                    .color(p.success),
                            );
                        });
                }
                let (w, h) = live.screen.lock().unwrap().size();
                let stats = live.outbound.stats();
                let info = match stats.encoding() {
                    Some(e) => format!("{e} · {w}×{h}"),
                    None => format!("{w}×{h}"),
                };
                ui.label(RichText::new(info).monospace().size(12.0).color(p.text2));
                if let Some(rtt) = stats.latency() {
                    let ms = rtt.as_millis();
                    let color = if ms > 150 {
                        ui.visuals().warn_fg_color
                    } else {
                        p.text2
                    };
                    ui.label(
                        RichText::new(format!("· {ms} ms"))
                            .monospace()
                            .size(12.0)
                            .color(color),
                    );
                }
                if live.bell_until.is_some_and(|t| Instant::now() < t) {
                    ui.label(
                        RichText::new(fill(tr("{} Bell"), &[&(char::from(Icon::Bell))]))
                            .color(p.accent_text)
                            .strong(),
                    );
                }
            },
        );

        ui.allocate_ui_with_layout(
            egui::vec2(center_w, H),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(((center_w - live.tools_w) / 2.0).max(0.0));
                let used = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        // Flat buttons: the Keys menu button would otherwise keep the default grey fill.
                        ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                        let scales = [
                            (Scale::Fit, tr("Fit").to_string(), tr("Fit to window")),
                            (
                                Scale::Actual,
                                "1:1".to_string(),
                                tr("Actual size (scroll bars)"),
                            ),
                        ];
                        theme::segmented(ui, &mut live.scale, &scales);
                        let (r, _) =
                            ui.allocate_exact_size(egui::vec2(1.0, 24.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 0.0, p.border);
                        let shortcut = if cfg!(target_os = "macos") {
                            "⌘⇧F"
                        } else {
                            tr("Ctrl+Shift+F")
                        };
                        let fs_icon = if fullscreen {
                            Icon::Minimize
                        } else {
                            Icon::Maximize
                        };
                        if tool(
                            ui,
                            fs_icon,
                            tr("Full screen"),
                            fullscreen,
                            compact,
                            shortcut,
                        ) {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                        }
                        let keys = if compact {
                            char::from(Icon::Keyboard).to_string()
                        } else {
                            fill(tr("{} Keys"), &[&(char::from(Icon::Keyboard))])
                        };
                        ui.menu_button(RichText::new(keys).color(p.text2), |ui| {
                            ui.set_min_width(175.0);
                            for (label, keys) in input::SHORTCUTS {
                                if ui.button(*label).clicked() {
                                    let _ = input::combo(&live.outbound, keys);
                                    ui.close();
                                }
                            }
                            ui.separator();
                            ui.label(RichText::new(tr("AltGr / ES symbols")).size(11.0).weak());
                            for row in input::ALTGR_SYMBOLS.chunks(5) {
                                ui.horizontal(|ui| {
                                    for &ch in row {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new(ch.to_string()).monospace(),
                                                )
                                                .min_size(egui::vec2(28.0, 24.0)),
                                            )
                                            .on_hover_text(fill(
                                                tr("Send '{}' to remote computer"),
                                                &[&ch],
                                            ))
                                            .clicked()
                                        {
                                            let _ = input::type_char(&live.outbound, ch);
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        })
                        .response
                        .on_hover_text(tr("Send key combinations and AltGr symbols"));
                        if tool(
                            ui,
                            Icon::ShieldAlert,
                            tr("Ctrl+Alt+Del"),
                            false,
                            compact,
                            tr("Send Ctrl+Alt+Del to the remote machine"),
                        ) {
                            let _ = input::ctrl_alt_del(&live.outbound);
                        }
                        if tool(
                            ui,
                            Icon::Clipboard,
                            tr("Clipboard"),
                            live.clipboard_view.open,
                            compact,
                            tr("View and manage synchronized clipboard text"),
                        ) {
                            live.clipboard_view.open = !live.clipboard_view.open;
                        }
                        if tool(
                            ui,
                            Icon::MessageCircle,
                            tr("Chat"),
                            live.chat.open,
                            compact,
                            tr("Chat with the person at the remote machine"),
                        ) {
                            live.chat.toggle(&live.outbound);
                        }
                        let files = if live.files.is_sftp_active() {
                            tr("Files (SFTP)")
                        } else {
                            tr("Files")
                        };
                        if tool(
                            ui,
                            Icon::Folder,
                            files,
                            live.files_view.open,
                            compact,
                            tr("Browse, download and upload files"),
                        ) {
                            live.files_view.open = !live.files_view.open;
                        }
                        if tool(
                            ui,
                            Icon::Eye,
                            tr("View only"),
                            live.view_only,
                            compact,
                            tr("Don't send keyboard, mouse or clipboard to the server"),
                        ) {
                            live.view_only = !live.view_only;
                        }
                    })
                    .response
                    .rect
                    .width();
                if (used - live.tools_w).abs() > 0.5 {
                    live.tools_w = used;
                    ui.ctx().request_repaint();
                }
            },
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    theme::danger(
                        ui,
                        fill(tr("{} Disconnect"), &[&(char::from(Icon::LogOut))]),
                    )
                    .min_size(egui::vec2(0.0, 36.0)),
                )
                .clicked()
            {
                live.user_disconnected = true;
                live.outbound.close();
            }
        });
    });
}
