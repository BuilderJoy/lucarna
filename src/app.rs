//! App shell: switches between the dashboard and a live session.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Layout, RichText};

use crate::features::chat::{ChatView, Msg};
use crate::features::clipboard::RemoteClipboard;
use crate::features::connection::{ConnectRequest, Dashboard, ServerAddress, SessionOptions, SshTarget};
use crate::features::file_transfer::{FilesView, RemoteFiles};
use crate::features::input::{self, InputForwarder};
use crate::features::remote_screen::{Framebuffer, Scale, ScreenView};
use crate::session::{self, Event, Outbound};

const PREVIEW_INTERVAL: Duration = Duration::from_secs(5);
const BELL_FLASH: Duration = Duration::from_secs(2);

struct LiveSession {
    address: ServerAddress,
    name: String,
    outbound: Arc<Outbound>,
    screen: Arc<Mutex<Framebuffer>>,
    view: ScreenView,
    input: InputForwarder,
    files: Arc<RemoteFiles>,
    files_view: FilesView,
    chat: ChatView,
    clipboard: Arc<RemoteClipboard>,
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
}

#[derive(Default)]
pub struct App {
    dashboard: Dashboard,
    pending: Option<(ServerAddress, SessionOptions)>,
    events: Option<mpsc::Receiver<Event>>,
    live: Option<LiveSession>,
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
        self.dashboard.last_request = Some(req.clone());
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let (address, repeater, creds) = (req.address.clone(), req.repeater, req.creds);
        self.pending = Some((req.address, req.options));
        std::thread::spawn(move || session::run(&address, repeater.as_ref(), ssh.as_ref(), &creds, tx, move || ctx.request_repaint()));
        self.events = Some(rx);
        self.dashboard.status.clear();
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.events else { return };
        let mut closed = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Connected {
                    name,
                    outbound,
                    screen,
                    files,
                    clipboard,
                } => {
                    self.dashboard.status.clear();
                    let (address, opts) = self
                        .pending
                        .take()
                        .unwrap_or_else(|| (ServerAddress::parse("127.0.0.1").unwrap(), SessionOptions::default()));
                    self.dashboard.mark_connected(&address);
                    if opts.fullscreen {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                    }
                    let scale = if opts.scale_1_1 { Scale::Actual } else { Scale::Fit };
                    let live = LiveSession {
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
                        view_only: opts.view_only,
                        share_clipboard: opts.share_clipboard,
                        save_preview: opts.save_preview,
                        last_preview: None,
                        bell_until: None,
                        user_disconnected: false,
                        scale,
                        bar_rect: None,
                        has_frame: false,
                    };
                    self.live = Some(live);
                }
                Event::Bell => {
                    if let Some(l) = &mut self.live {
                        l.bell_until = Some(Instant::now() + BELL_FLASH);
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
                    ctx.request_repaint_after(BELL_FLASH);
                }
                Event::Chat(msg) => {
                    if matches!(msg, Msg::Text(_)) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
                    }
                    if let Some(l) = &mut self.live {
                        l.chat.apply(msg);
                    }
                }
                Event::Files(update) => {
                    if let Some(l) = &mut self.live {
                        l.files_view.apply(update, &l.files);
                    }
                }
                Event::ScreenUpdated => {
                    if let Some(l) = &mut self.live {
                        l.has_frame = true;
                        if l.save_preview && l.last_preview.is_none_or(|t| t.elapsed() >= PREVIEW_INTERVAL) {
                            let (w, h, rgba) = l.screen.lock().unwrap().thumbnail(320, 200);
                            self.dashboard.update_preview(ctx, &l.address, w, h, &rgba);
                            l.last_preview = Some(Instant::now());
                        }
                    }
                }
                Event::ServerClipboard(text) => {
                    if self.live.as_ref().is_some_and(|l| l.share_clipboard && !l.view_only) {
                        ctx.copy_text(text);
                    }
                }
                Event::Closed(reason) => {
                    let intentional = self.live.as_ref().is_some_and(|l| l.user_disconnected);
                    if let Some(l) = self.live.take()
                        && l.has_frame
                        && l.save_preview
                    {
                        let (w, h, rgba) = l.screen.lock().unwrap().thumbnail(320, 200);
                        self.dashboard.update_preview(ctx, &l.address, w, h, &rgba);
                    }
                    if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    }
                    if intentional {
                        self.dashboard.status.clear();
                    } else {
                        self.dashboard.status = reason;
                    }
                    self.pending = None;
                    closed = true;
                }
            }
        }
        if closed {
            self.events = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_events(&ctx);
        let Some(live) = &mut self.live else {
            if let Some(req) = self.dashboard.show(ui, self.events.is_some()) {
                self.connect(&ctx, req);
            }
            return;
        };
        // Our own shortcut: consumed before input forwarding so the remote machine never sees it.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let toggle = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::F));
        if toggle {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
        let bar = egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(12, 6));
        if !fullscreen {
            live.bar_rect = None;
            egui::Panel::top("bar").frame(bar).show(ui, |ui| bar_contents(ui, live, fullscreen));
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
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ui, |ui| {
            if !live.has_frame {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 2.0 - 40.0);
                    ui.add(egui::Spinner::new().size(32.0));
                    ui.add_space(8.0);
                    ui.label(RichText::new("Waiting for the remote screen…").weak());
                });
                return;
            }
            let viewport = live.view.show(ui, &live.screen, live.scale);
            // Clicks on our own windows and typing in our text inputs must not reach the remote machine.
            let over_ui = [live.files_view.rect, live.chat.rect, live.bar_rect]
                .into_iter()
                .flatten()
                .any(|r| ui.input(|i| i.pointer.hover_pos().is_some_and(|p| r.contains(p))));
            live.view.draw_cursor(ui, &viewport, !over_ui);
            if !over_ui && !live.view_only && !ui.ctx().egui_wants_keyboard_input() {
                if live.share_clipboard {
                    live.clipboard.forward_paste(ui);
                }
                live.input.forward(ui, &viewport, &live.outbound);
            }
        });
        live.files_view.show(&ctx, &live.files);
        live.chat.show(&ctx, &live.outbound);
    }
}

fn bar_contents(ui: &mut egui::Ui, live: &mut LiveSession, fullscreen: bool) {
    use lucide_icons::Icon;
    ui.horizontal(|ui| {
        let dot = if live.has_frame {
            Color32::from_rgb(80, 200, 120)
        } else {
            ui.visuals().warn_fg_color
        };
        ui.label(RichText::new("●").color(dot));
        ui.label(RichText::new(&live.name).strong());
        let (w, h) = live.screen.lock().unwrap().size();
        ui.label(RichText::new(format!("{w}×{h}")).weak());
        if live.bell_until.is_some_and(|t| Instant::now() < t) {
            let accent = ui.visuals().selection.bg_fill;
            ui.label(RichText::new(format!("{} Bell", char::from(Icon::Bell))).color(accent).strong());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button(format!("{} Disconnect", char::from(Icon::LogOut))).clicked() {
                live.user_disconnected = true;
                live.outbound.close();
            }
            if ui
                .selectable_label(live.files_view.open, format!("{} Files", char::from(Icon::Folder)))
                .on_hover_text("Browse, download and upload files")
                .clicked()
            {
                live.files_view.open = !live.files_view.open;
            }
            if ui
                .selectable_label(live.chat.open, format!("{} Chat", char::from(Icon::MessageCircle)))
                .on_hover_text("Chat with the person at the remote machine")
                .clicked()
            {
                live.chat.toggle(&live.outbound);
            }
            ui.menu_button(format!("{} Keys", char::from(Icon::Keyboard)), |ui| {
                ui.set_min_width(175.0);
                for (label, keys) in input::SHORTCUTS {
                    if ui.button(*label).clicked() {
                        let _ = input::combo(&live.outbound, keys);
                        ui.close();
                    }
                }
                ui.separator();
                ui.label(RichText::new("AltGr / ES symbols").size(11.0).weak());
                for row in input::ALTGR_SYMBOLS.chunks(5) {
                    ui.horizontal(|ui| {
                        for &ch in row {
                            if ui
                                .add(egui::Button::new(RichText::new(ch.to_string()).monospace()).min_size(egui::vec2(28.0, 24.0)))
                                .on_hover_text(format!("Send '{ch}' to remote computer"))
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
            .on_hover_text("Send key combinations and AltGr symbols");
            if ui
                .button(format!("{} Ctrl+Alt+Del", char::from(Icon::ShieldAlert)))
                .on_hover_text("Send Ctrl+Alt+Del to the remote machine")
                .clicked()
            {
                let _ = input::ctrl_alt_del(&live.outbound);
            }
            ui.separator();
            let shortcut = if cfg!(target_os = "macos") { "⌘⇧F" } else { "Ctrl+Shift+F" };
            let fs_icon = char::from(if fullscreen { Icon::Minimize } else { Icon::Maximize });
            if ui.selectable_label(fullscreen, format!("{fs_icon} Full screen")).on_hover_text(shortcut).clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            let (label, tip) = match live.scale {
                Scale::Fit => ("Fit", "Fit to window. Click for actual size (1:1, scroll bars)"),
                Scale::Actual => ("1:1", "Actual size. Click to fit the window"),
            };
            if ui.button(format!("{} {label}", char::from(Icon::ZoomIn))).on_hover_text(tip).clicked() {
                live.scale = if live.scale == Scale::Fit { Scale::Actual } else { Scale::Fit };
            }
            if ui
                .selectable_label(live.view_only, format!("{} View only", char::from(Icon::Eye)))
                .on_hover_text("Don't send keyboard, mouse or clipboard to the server")
                .clicked()
            {
                live.view_only = !live.view_only;
            }
        });
    });
}

/// Dark theme with the amber accent of the logo and the embedded Lucide icon font.
pub fn apply_theme(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "lucide".into(),
        Arc::new(egui::FontData::from_static(lucide_icons::LUCIDE_FONT_BYTES).tweak(egui::FontTweak {
            y_offset_factor: 0.08,
            ..Default::default()
        })),
    );
    if let Some(prop) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        prop.insert(1, "lucide".into());
    }
    if let Some(mono) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        mono.push("lucide".into());
    }
    ctx.set_fonts(fonts);

    let accent = Color32::from_rgb(0xff, 0xbe, 0x3d);
    let mut v = egui::Visuals::dark();
    v.selection.bg_fill = accent;
    v.selection.stroke.color = Color32::BLACK;
    v.hyperlink_color = accent;
    v.panel_fill = Color32::from_rgb(0x10, 0x16, 0x22);
    v.window_fill = Color32::from_rgb(0x18, 0x21, 0x33);
    v.extreme_bg_color = Color32::from_rgb(0x0b, 0x10, 0x1a);
    v.widgets.hovered.bg_stroke.color = accent;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals(v);
}
