//! User-space keyboard and mouse event injection with local mouse priority (Anti-RAT guard).

use std::io;
use std::time::{Duration, Instant};

const SHIFT: u32 = 0xffe1;
const CONTROL: u32 = 0xffe3;
const ALT: u32 = 0xffe9;
const ALT_GR: u32 = 0xfe03;
const DELETE: u32 = 0xffff;
const ESCAPE: u32 = 0xff1b;
const TAB: u32 = 0xff09;
const PRINT: u32 = 0xff61;
const SUPER: u32 = 0xffeb;

/// Default window during which physical local mouse movement suppresses remote input.
pub const LOCAL_PRIORITY_COOLDOWN: Duration = Duration::from_secs(2);

/// An RFB client input event received by the Host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostInputEvent {
    Key { down: bool, keysym: u32 },
    Pointer { buttons: u8, x: u16, y: u16 },
}

/// Decoded platform-neutral key representation translated from an X11 keysym.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeKey {
    Char(char),
    Backspace,
    Tab,
    Enter,
    Escape,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowLeft,
    ArrowUp,
    ArrowRight,
    ArrowDown,
    PrintScreen,
    Function(u8),
    Shift,
    Control,
    Alt,
    AltGr,
    Super,
}

impl NativeKey {
    /// Translates an RFB X11 keysym (RFC 6143 §7.5.4) into a `NativeKey`.
    pub fn from_keysym(keysym: u32) -> Option<Self> {
        Some(match keysym {
            0xff08 => Self::Backspace,
            TAB => Self::Tab,
            0xff0d | 0xff8d => Self::Enter,
            ESCAPE => Self::Escape,
            DELETE => Self::Delete,
            0xff63 => Self::Insert,
            0xff50 => Self::Home,
            0xff51 => Self::ArrowLeft,
            0xff52 => Self::ArrowUp,
            0xff53 => Self::ArrowRight,
            0xff54 => Self::ArrowDown,
            0xff55 => Self::PageUp,
            0xff56 => Self::PageDown,
            0xff57 => Self::End,
            PRINT => Self::PrintScreen,
            0xffbe..=0xffc9 => Self::Function((keysym - 0xffbe + 1) as u8),
            SHIFT | 0xffe2 => Self::Shift,
            CONTROL | 0xffe4 => Self::Control,
            ALT | 0xffea => Self::Alt,
            ALT_GR => Self::AltGr,
            SUPER | 0xffec => Self::Super,
            cp @ 0x20..=0x7e | cp @ 0xa0..=0xff => Self::Char(char::from_u32(cp)?),
            cp if (cp & 0xff00_0000) == 0x0100_0000 => {
                Self::Char(char::from_u32(cp & 0x00ff_ffff)?)
            }
            _ => return None,
        })
    }
}

/// Decoded pointer button state from the RFB 8-bit button mask (RFC 6143 §7.5.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerButtons {
    pub left: bool,
    pub middle: bool,
    pub right: bool,
    pub wheel_up: bool,
    pub wheel_down: bool,
}

impl PointerButtons {
    pub fn from_mask(mask: u8) -> Self {
        Self {
            left: mask & (1 << 0) != 0,
            middle: mask & (1 << 1) != 0,
            right: mask & (1 << 2) != 0,
            wheel_up: mask & (1 << 3) != 0,
            wheel_down: mask & (1 << 4) != 0,
        }
    }
}

/// Protects the local operator: when the physical mouse moves locally, remote input is suppressed
/// until `cooldown` expires.
#[derive(Debug)]
pub struct LocalActivityGuard {
    last_local_pos: Option<(u16, u16)>,
    last_local_move: Option<Instant>,
    cooldown: Duration,
}

impl Default for LocalActivityGuard {
    fn default() -> Self {
        Self::new(LOCAL_PRIORITY_COOLDOWN)
    }
}

impl LocalActivityGuard {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            last_local_pos: None,
            last_local_move: None,
            cooldown,
        }
    }

    /// Records the current physical cursor position on the host machine.
    pub fn notify_local_pointer(&mut self, pos: (u16, u16), now: Instant) {
        if self.last_local_pos.is_some_and(|prev| prev != pos) {
            self.last_local_move = Some(now);
        }
        self.last_local_pos = Some(pos);
    }

    /// Returns true if remote input must be blocked because the local operator moved the mouse recently.
    pub fn is_remote_blocked(&self, now: Instant) -> bool {
        self.last_local_move
            .is_some_and(|t| now.saturating_duration_since(t) < self.cooldown)
    }
}

/// Sink for injecting remote keyboard and pointer events in user space.
pub trait InputInjector: Send {
    fn inject(&mut self, event: HostInputEvent) -> io::Result<()>;
}

/// Records decoded events in memory, enforcing `view_only` and `LocalActivityGuard`.
#[derive(Default)]
pub struct RecordingInjector {
    pub view_only: bool,
    pub guard: LocalActivityGuard,
    pub events: Vec<HostInputEvent>,
    pub decoded_keys: Vec<(bool, NativeKey)>,
}

impl RecordingInjector {
    pub fn new(view_only: bool) -> Self {
        Self {
            view_only,
            guard: LocalActivityGuard::default(),
            events: Vec::new(),
            decoded_keys: Vec::new(),
        }
    }
}

impl InputInjector for RecordingInjector {
    fn inject(&mut self, event: HostInputEvent) -> io::Result<()> {
        if self.view_only || self.guard.is_remote_blocked(Instant::now()) {
            return Ok(());
        }
        if let HostInputEvent::Key { down, keysym } = event
            && let Some(k) = NativeKey::from_keysym(keysym)
        {
            self.decoded_keys.push((down, k));
        }
        self.events.push(event);
        Ok(())
    }
}

/// User-space OS input injector with local mouse priority and view-only enforcement.
pub struct SystemInputInjector {
    pub view_only: bool,
    pub guard: LocalActivityGuard,
    last_buttons: PointerButtons,
}

impl SystemInputInjector {
    pub fn new(view_only: bool) -> Self {
        Self {
            view_only,
            guard: LocalActivityGuard::default(),
            last_buttons: PointerButtons::default(),
        }
    }
}

impl InputInjector for SystemInputInjector {
    fn inject(&mut self, event: HostInputEvent) -> io::Result<()> {
        if self.view_only || self.guard.is_remote_blocked(Instant::now()) {
            return Ok(());
        }
        match event {
            HostInputEvent::Pointer { buttons, x, y } => {
                let b = PointerButtons::from_mask(buttons);
                let prev = self.last_buttons;
                self.last_buttons = b;
                inject_os_pointer(x, y, prev, b);
            }
            HostInputEvent::Key { down, keysym } => {
                if let Some(key) = NativeKey::from_keysym(keysym) {
                    inject_os_key(down, key);
                }
            }
        }
        Ok(())
    }
}

fn inject_os_pointer(x: u16, y: u16, prev: PointerButtons, buttons: PointerButtons) {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let _ = Command::new("xdotool")
            .args(["mousemove", &x.to_string(), &y.to_string()])
            .status();
        for (btn_num, was_down, is_down) in [
            ("1", prev.left, buttons.left),
            ("2", prev.middle, buttons.middle),
            ("3", prev.right, buttons.right),
        ] {
            if is_down && !was_down {
                let _ = Command::new("xdotool")
                    .args(["mousedown", btn_num])
                    .status();
            } else if !is_down && was_down {
                let _ = Command::new("xdotool").args(["mouseup", btn_num]).status();
            }
        }
        if buttons.wheel_up {
            let _ = Command::new("xdotool").args(["click", "4"]).status();
        }
        if buttons.wheel_down {
            let _ = Command::new("xdotool").args(["click", "5"]).status();
        }
    }
    let _ = (x, y, prev, buttons);
}

fn inject_os_key(down: bool, key: NativeKey) {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        if !down {
            return;
        }
        match key {
            NativeKey::Char(c) => {
                let _ = Command::new("xdotool")
                    .args(["type", "--delay", "0", "--", &c.to_string()])
                    .status();
            }
            other => {
                let sym = match other {
                    NativeKey::Backspace => Some("BackSpace"),
                    NativeKey::Tab => Some("Tab"),
                    NativeKey::Enter => Some("Return"),
                    NativeKey::Escape => Some("Escape"),
                    NativeKey::Delete => Some("Delete"),
                    NativeKey::Insert => Some("Insert"),
                    NativeKey::Home => Some("Home"),
                    NativeKey::End => Some("End"),
                    NativeKey::PageUp => Some("Prior"),
                    NativeKey::PageDown => Some("Next"),
                    NativeKey::ArrowLeft => Some("Left"),
                    NativeKey::ArrowUp => Some("Up"),
                    NativeKey::ArrowRight => Some("Right"),
                    NativeKey::ArrowDown => Some("Down"),
                    _ => None,
                };
                if let Some(s) = sym {
                    let _ = Command::new("xdotool").args(["key", s]).status();
                }
            }
        }
    }
    let _ = (down, key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_ascii_unicode_and_special_keysyms() {
        assert_eq!(NativeKey::from_keysym(0x41), Some(NativeKey::Char('A')));
        assert_eq!(
            NativeKey::from_keysym(0x0100_20ac),
            Some(NativeKey::Char('€'))
        );
        assert_eq!(NativeKey::from_keysym(0xff0d), Some(NativeKey::Enter));
        assert_eq!(NativeKey::from_keysym(0xffc1), Some(NativeKey::Function(4)));
        assert_eq!(NativeKey::from_keysym(CONTROL), Some(NativeKey::Control));
        assert_eq!(NativeKey::from_keysym(ALT_GR), Some(NativeKey::AltGr));
    }

    #[test]
    fn local_mouse_priority_blocks_remote_injection_during_cooldown() {
        let mut guard = LocalActivityGuard::new(Duration::from_secs(2));
        let t0 = Instant::now();
        guard.notify_local_pointer((100, 100), t0);
        assert!(!guard.is_remote_blocked(t0));

        // Physical operator moves the mouse to (120, 115): remote input is immediately blocked.
        guard.notify_local_pointer((120, 115), t0);
        assert!(guard.is_remote_blocked(t0 + Duration::from_millis(500)));
        // After cooldown (2s), remote input is allowed again.
        assert!(!guard.is_remote_blocked(t0 + Duration::from_secs(3)));
    }

    #[test]
    fn view_only_suppresses_all_injected_events() {
        let mut inj = RecordingInjector::new(true);
        inj.inject(HostInputEvent::Key {
            down: true,
            keysym: 0x41,
        })
        .unwrap();
        inj.inject(HostInputEvent::Pointer {
            buttons: 1,
            x: 50,
            y: 60,
        })
        .unwrap();
        assert!(inj.events.is_empty());
    }
}
