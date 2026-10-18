use eframe::egui::{self, Event, Modifiers};

use super::{keysym, protocol};
use crate::features::remote_screen::Viewport;
use crate::session::Outbound;

/// Tracks what the server last saw so only changes are sent.
#[derive(Default)]
pub struct InputForwarder {
    buttons: u8,
    pos: (u16, u16),
    mods: Modifiers,
}

impl InputForwarder {
    // Send errors surface through the session reader as a Closed event, so they are ignored here.
    pub fn forward(&mut self, ui: &egui::Ui, viewport: &Viewport, out: &Outbound) {
        ui.input(|i| {
            let m = i.modifiers;
            // On macOS, Cmd acts as Ctrl on the remote side.
            for (was, is, sym) in [
                (self.mods.shift, m.shift, keysym::SHIFT),
                (self.mods.ctrl || self.mods.mac_cmd, m.ctrl || m.mac_cmd, keysym::CONTROL),
                (self.mods.alt, m.alt, keysym::ALT),
            ] {
                if was != is {
                    let _ = protocol::key(out, is, sym);
                }
            }
            self.mods = m;

            if let Some(p) = i.pointer.hover_pos().filter(|p| viewport.contains(*p)) {
                let buttons =
                    i.pointer.primary_down() as u8 | (i.pointer.middle_down() as u8) << 1 | (i.pointer.secondary_down() as u8) << 2;
                let pos = viewport.to_remote(p);
                if (buttons, pos) != (self.buttons, self.pos) {
                    let _ = protocol::pointer(out, buttons, pos);
                    (self.buttons, self.pos) = (buttons, pos);
                }
            }

            for event in &i.events {
                match event {
                    Event::Text(text) if !(m.ctrl || m.mac_cmd) => {
                        // On macOS, Option+key (e.g. Option+2 → '@' on Spanish layouts) holds Alt while emitting
                        // the composed character. Release remote Alt around the character tap so Windows VNC servers
                        // can synthesize their own AltGr (Ctrl+Alt) state for that keysym instead of seeing Alt+char.
                        if m.alt {
                            let _ = protocol::key(out, false, keysym::ALT);
                        }
                        for c in text.chars() {
                            let _ = protocol::tap(out, keysym::from_char(c));
                        }
                        if m.alt {
                            let _ = protocol::key(out, true, keysym::ALT);
                        }
                    }
                    Event::Key {
                        key, pressed, modifiers, ..
                    } => {
                        let sym = keysym::from_special_key(*key).or_else(|| {
                            let name = key.name();
                            (name.len() == 1 && (modifiers.ctrl || modifiers.mac_cmd))
                                .then(|| name.to_ascii_lowercase().as_bytes()[0] as u32)
                        });
                        if let Some(sym) = sym {
                            let _ = protocol::key(out, *pressed, sym);
                        }
                    }
                    // egui turns Cmd/Ctrl+C/X/V into these instead of Key events; Ctrl is already held.
                    Event::Copy => drop(protocol::tap(out, 'c' as u32)),
                    Event::Cut => drop(protocol::tap(out, 'x' as u32)),
                    Event::Paste(_) => drop(protocol::tap(out, 'v' as u32)),
                    Event::MouseWheel { delta, .. } if delta.y != 0.0 => {
                        let bit = if delta.y > 0.0 { 8 } else { 16 };
                        let _ = protocol::pointer(out, self.buttons | bit, self.pos);
                        let _ = protocol::pointer(out, self.buttons, self.pos);
                    }
                    _ => {}
                }
            }
        });
    }
}
