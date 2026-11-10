//! User-space keyboard and mouse event injection with local mouse priority (Anti-RAT guard).

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::os_input::{self, Button, Mods};

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

    /// The local operator moved the physical mouse (seen as the cursor not being where we left it).
    pub fn local_moved(&mut self, now: Instant) {
        self.last_local_move = Some(now);
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

    /// The size of the shared picture changed; injectors that need it to place the pointer keep it.
    fn set_frame_size(&mut self, _width: u16, _height: u16) {}
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

/// User-space OS input injector with local mouse priority and view-only enforcement. The system calls live in
/// `os_input`, one implementation per operating system.
pub struct SystemInputInjector {
    pub view_only: bool,
    pub guard: LocalActivityGuard,
    last_buttons: PointerButtons,
    mods: Mods,
    /// Size of the shared picture, to turn its coordinates into the system's units.
    frame: (u16, u16),
    /// Last button press (time, position, count), to give the system double and triple clicks.
    last_click: Option<(Instant, (i32, i32), i64)>,
    /// Where the cursor was right after our last injected move. If it is somewhere else when the next remote
    /// move arrives, a person moved the mouse. ponytail: on Linux this is one `xdotool` spawn per probe; read
    /// the pointer through the X11 API if the cost shows.
    probe: fn() -> Option<(u16, u16)>,
    expected: Option<(u16, u16)>,
    /// Unix milliseconds of the last remote event we dropped because the person here had the mouse (0 = never).
    blocked_at: Option<Arc<AtomicU64>>,
    /// False in tests: the decisions run but nothing is sent to the operating system (a test must not move
    /// the mouse of whoever runs it).
    real: bool,
}

impl SystemInputInjector {
    pub fn new(view_only: bool) -> Self {
        Self {
            view_only,
            guard: LocalActivityGuard::default(),
            last_buttons: PointerButtons::default(),
            mods: Mods::default(),
            frame: (0, 0),
            last_click: None,
            probe: os_input::pointer,
            expected: None,
            blocked_at: None,
            real: true,
        }
    }

    /// Lets the UI see when remote input was held back.
    pub fn with_blocked_marker(mut self, marker: Option<Arc<AtomicU64>>) -> Self {
        self.blocked_at = marker;
        self
    }

    /// 1 for a new click, 2 for a double click (same place, within 400 ms), and so on.
    fn click_count(&mut self, at: (i32, i32), now: Instant) -> i64 {
        let n = match self.last_click {
            Some((t, p, n))
                if now.saturating_duration_since(t) < Duration::from_millis(400)
                    && (p.0 - at.0).abs() <= 4
                    && (p.1 - at.1).abs() <= 4 =>
            {
                n + 1
            }
            _ => 1,
        };
        self.last_click = Some((now, at, n));
        n
    }
}

impl InputInjector for SystemInputInjector {
    fn set_frame_size(&mut self, width: u16, height: u16) {
        self.frame = (width, height);
    }

    fn inject(&mut self, event: HostInputEvent) -> io::Result<()> {
        if self.view_only {
            return Ok(());
        }
        let now = Instant::now();
        if matches!(event, HostInputEvent::Pointer { .. })
            && let (Some(here), Some(left)) = ((self.probe)(), self.expected)
            && here != left
        {
            self.guard.local_moved(now);
            // Judge the next remote event against where the person left the cursor, not where we did.
            self.expected = Some(here);
        }
        if self.guard.is_remote_blocked(now) {
            if let Some(m) = &self.blocked_at {
                let ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                m.store(ms, Ordering::Relaxed);
            }
            return Ok(());
        }
        match event {
            HostInputEvent::Pointer { buttons, x, y } => {
                let b = PointerButtons::from_mask(buttons);
                let prev = self.last_buttons;
                self.last_buttons = b;
                let at = match self.real.then(os_input::screen_size).flatten() {
                    Some(os) => os_input::scale_point(x, y, self.frame, os),
                    None => (i32::from(x), i32::from(y)),
                };
                let held = [
                    (b.left, Button::Left),
                    (b.right, Button::Right),
                    (b.middle, Button::Middle),
                ]
                .into_iter()
                .find_map(|(down, btn)| down.then_some(btn));
                if self.real {
                    os_input::pointer_to(at.0, at.1, held, self.mods);
                }
                for (btn, was, is) in [
                    (Button::Left, prev.left, b.left),
                    (Button::Middle, prev.middle, b.middle),
                    (Button::Right, prev.right, b.right),
                ] {
                    if is && !was {
                        let clicks = self.click_count(at, now);
                        if self.real {
                            os_input::button(btn, true, clicks, at, self.mods);
                        }
                    } else if !is && was {
                        let clicks = self.last_click.map_or(1, |c| c.2);
                        if self.real {
                            os_input::button(btn, false, clicks, at, self.mods);
                        }
                    }
                }
                // The wheel arrives as a press and a release: act on the press only.
                if self.real && b.wheel_up && !prev.wheel_up {
                    os_input::wheel(true, self.mods);
                }
                if self.real && b.wheel_down && !prev.wheel_down {
                    os_input::wheel(false, self.mods);
                }
                self.expected = (self.probe)();
            }
            HostInputEvent::Key { down, keysym } => {
                if let Some(key) = NativeKey::from_keysym(keysym) {
                    self.mods.update(key, down);
                    if self.real {
                        os_input::key(down, key, self.mods);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static FAKE_X: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn fake_pointer() -> Option<(u16, u16)> {
        let x = FAKE_X.load(Ordering::SeqCst) as u16;
        Some((x, x))
    }

    #[test]
    fn a_person_moving_the_mouse_holds_back_remote_input_for_the_cooldown() {
        let marker = Arc::new(AtomicU64::new(0));
        let mut inj = SystemInputInjector::new(false).with_blocked_marker(Some(marker.clone()));
        inj.probe = fake_pointer;
        inj.real = false;
        inj.guard = LocalActivityGuard::new(Duration::from_millis(80));
        let remote = |x| HostInputEvent::Pointer {
            buttons: 0,
            x,
            y: x,
        };

        // The remote moves the cursor; it lands where it was told to, so nobody else touched it.
        FAKE_X.store(10, Ordering::SeqCst);
        inj.inject(remote(10)).unwrap();
        inj.inject(remote(10)).unwrap();
        assert_eq!(marker.load(Ordering::Relaxed), 0);

        // A person moves the mouse: the next remote move is dropped and the UI marker is set.
        FAKE_X.store(300, Ordering::SeqCst);
        inj.inject(remote(11)).unwrap();
        assert_ne!(marker.load(Ordering::Relaxed), 0);

        // Still inside the pause: still held back, and it does not re-trigger by itself.
        inj.inject(remote(12)).unwrap();
        assert!(inj.guard.is_remote_blocked(Instant::now()));

        // After the pause the remote has the mouse again, with no new local movement to blame.
        std::thread::sleep(Duration::from_millis(120));
        marker.store(0, Ordering::Relaxed);
        inj.inject(remote(13)).unwrap();
        assert_eq!(
            marker.load(Ordering::Relaxed),
            0,
            "still blocked after the pause"
        );
        assert!(!inj.guard.is_remote_blocked(Instant::now()));
    }

    #[test]
    fn view_only_never_reaches_the_probe_or_the_marker() {
        let marker = Arc::new(AtomicU64::new(0));
        let mut inj = SystemInputInjector::new(true).with_blocked_marker(Some(marker.clone()));
        inj.probe = fake_pointer;
        inj.real = false;
        inj.inject(HostInputEvent::Pointer {
            buttons: 0,
            x: 1,
            y: 1,
        })
        .unwrap();
        assert_eq!(inj.expected, None);
        assert_eq!(marker.load(Ordering::Relaxed), 0);
    }

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
