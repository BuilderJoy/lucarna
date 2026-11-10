//! The operating-system side of remote control: screen size, cursor position, mouse and keyboard injection, and
//! the permissions each system asks for. Plain FFI to the system libraries, no crates (Windows: user32 and
//! gdi32; macOS: CoreGraphics; Linux: the `xdotool` and `import` commands).
//!
//! The tables that turn a key into a system key code are ordinary functions so they are tested on every system.

use std::io;

use super::capture::Frame;
use super::input_inject::NativeKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// Modifier keys currently held by the remote person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub alt_gr: bool,
    pub meta: bool,
}

impl Mods {
    /// Follows a key press or release; returns true when `key` was a modifier.
    pub fn update(&mut self, key: NativeKey, down: bool) -> bool {
        match key {
            NativeKey::Shift => self.shift = down,
            NativeKey::Control => self.control = down,
            NativeKey::Alt => self.alt = down,
            NativeKey::AltGr => self.alt_gr = down,
            NativeKey::Super => self.meta = down,
            _ => return false,
        }
        true
    }
}

/// Something the person sharing has to do for remote control to work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setup {
    /// macOS: Privacy & Security > Screen Recording.
    ScreenRecording,
    /// macOS: Privacy & Security > Accessibility.
    Accessibility,
    /// Linux: `xdotool` and ImageMagick's `import` are not installed.
    LinuxTools,
}

/// Maps a point of the shared picture to the system's own units, which differ on a scaled screen (macOS points
/// against captured pixels). Rounds to the nearest unit and stays inside the screen.
pub fn scale_point(x: u16, y: u16, frame: (u16, u16), os: (u32, u32)) -> (i32, i32) {
    if frame.0 == 0 || frame.1 == 0 || os.0 == 0 || os.1 == 0 {
        return (i32::from(x), i32::from(y));
    }
    let sx = (f64::from(x) * f64::from(os.0) / f64::from(frame.0)).round() as i64;
    let sy = (f64::from(y) * f64::from(os.1) / f64::from(frame.1)).round() as i64;
    (
        sx.clamp(0, i64::from(os.0) - 1) as i32,
        sy.clamp(0, i64::from(os.1) - 1) as i32,
    )
}

/// macOS virtual key code of a non-character key (`kVK_*`). Characters travel as Unicode instead.
pub fn mac_keycode(key: NativeKey) -> Option<u16> {
    Some(match key {
        NativeKey::Enter => 36,
        NativeKey::Tab => 48,
        NativeKey::Backspace => 51,
        NativeKey::Escape => 53,
        NativeKey::Delete => 117,
        NativeKey::Insert => 114,
        NativeKey::Home => 115,
        NativeKey::End => 119,
        NativeKey::PageUp => 116,
        NativeKey::PageDown => 121,
        NativeKey::ArrowLeft => 123,
        NativeKey::ArrowRight => 124,
        NativeKey::ArrowDown => 125,
        NativeKey::ArrowUp => 126,
        NativeKey::Function(n) => match n {
            1 => 122,
            2 => 120,
            3 => 99,
            4 => 118,
            5 => 96,
            6 => 97,
            7 => 98,
            8 => 100,
            9 => 101,
            10 => 109,
            11 => 103,
            12 => 111,
            _ => return None,
        },
        _ => return None,
    })
}

/// Windows virtual key code and whether it is an "extended" key.
pub fn windows_vk(key: NativeKey) -> Option<(u16, bool)> {
    Some(match key {
        NativeKey::Backspace => (0x08, false),
        NativeKey::Tab => (0x09, false),
        NativeKey::Enter => (0x0d, false),
        NativeKey::Escape => (0x1b, false),
        NativeKey::PageUp => (0x21, true),
        NativeKey::PageDown => (0x22, true),
        NativeKey::End => (0x23, true),
        NativeKey::Home => (0x24, true),
        NativeKey::ArrowLeft => (0x25, true),
        NativeKey::ArrowUp => (0x26, true),
        NativeKey::ArrowRight => (0x27, true),
        NativeKey::ArrowDown => (0x28, true),
        NativeKey::PrintScreen => (0x2c, false),
        NativeKey::Insert => (0x2d, true),
        NativeKey::Delete => (0x2e, true),
        NativeKey::Function(n @ 1..=12) => (0x70 + u16::from(n) - 1, false),
        NativeKey::Function(_) => return None,
        NativeKey::Shift => (0x10, false),
        NativeKey::Control => (0x11, false),
        NativeKey::Alt => (0x12, false),
        NativeKey::AltGr => (0xa5, true),
        NativeKey::Super => (0x5b, true),
        NativeKey::Char(_) => return None,
    })
}

// ---------------------------------------------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod sys {
    use super::*;
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(event: *mut c_void) -> Point;
        fn CGEventCreateMouseEvent(
            source: *const c_void,
            kind: u32,
            at: Point,
            button: u32,
        ) -> *mut c_void;
        fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
        fn CGEventKeyboardSetUnicodeString(event: *mut c_void, len: usize, text: *const u16);
        fn CGEventCreateScrollWheelEvent2(
            source: *const c_void,
            units: u32,
            count: u32,
            w1: i32,
            w2: i32,
            w3: i32,
        ) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventSetIntegerValueField(event: *mut c_void, field: u32, value: i64);
        fn CGEventPost(tap: u32, event: *mut c_void);
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(display: u32) -> Rect;
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *const c_void);
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    const HID_TAP: u32 = 0;
    const CLICK_STATE: u32 = 1;

    fn flags(m: Mods) -> u64 {
        let mut f = 0;
        if m.shift {
            f |= 0x0002_0000;
        }
        if m.control {
            f |= 0x0004_0000;
        }
        if m.alt || m.alt_gr {
            f |= 0x0008_0000;
        }
        if m.meta {
            f |= 0x0010_0000;
        }
        f
    }

    /// Posts and releases an event; a null event (permission or memory failure) is ignored.
    unsafe fn post(event: *mut c_void, m: Mods) {
        if event.is_null() {
            return;
        }
        unsafe {
            CGEventSetFlags(event, flags(m));
            CGEventPost(HID_TAP, event);
            CFRelease(event);
        }
    }

    pub fn screen_size() -> Option<(u32, u32)> {
        let r = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        (r.size.width >= 1.0 && r.size.height >= 1.0)
            .then_some((r.size.width as u32, r.size.height as u32))
    }

    pub fn pointer() -> Option<(u16, u16)> {
        unsafe {
            let e = CGEventCreate(std::ptr::null());
            if e.is_null() {
                return None;
            }
            let p = CGEventGetLocation(e);
            CFRelease(e);
            Some((p.x.max(0.0) as u16, p.y.max(0.0) as u16))
        }
    }

    pub fn pointer_to(x: i32, y: i32, held: Option<Button>, m: Mods) {
        // A move with a button down must be a drag, or the other application never sees the drag.
        let (kind, button) = match held {
            None => (5, 0),
            Some(Button::Left) => (6, 0),
            Some(Button::Right) => (7, 1),
            Some(Button::Middle) => (27, 2),
        };
        let at = Point {
            x: f64::from(x),
            y: f64::from(y),
        };
        unsafe {
            post(
                CGEventCreateMouseEvent(std::ptr::null(), kind, at, button),
                m,
            )
        };
    }

    pub fn button(b: Button, down: bool, clicks: i64, at: (i32, i32), m: Mods) {
        let (kind, button) = match (b, down) {
            (Button::Left, true) => (1, 0),
            (Button::Left, false) => (2, 0),
            (Button::Right, true) => (3, 1),
            (Button::Right, false) => (4, 1),
            (Button::Middle, true) => (25, 2),
            (Button::Middle, false) => (26, 2),
        };
        let p = Point {
            x: f64::from(at.0),
            y: f64::from(at.1),
        };
        unsafe {
            let e = CGEventCreateMouseEvent(std::ptr::null(), kind, p, button);
            if !e.is_null() {
                CGEventSetIntegerValueField(e, CLICK_STATE, clicks.max(1));
            }
            post(e, m);
        }
    }

    pub fn wheel(up: bool, m: Mods) {
        let delta = if up { 3 } else { -3 };
        unsafe {
            post(
                CGEventCreateScrollWheelEvent2(std::ptr::null(), 1, 1, delta, 0, 0),
                m,
            )
        };
    }

    pub fn key(down: bool, key: NativeKey, m: Mods) {
        match key {
            // Held modifiers travel as flags on every event.
            NativeKey::Shift
            | NativeKey::Control
            | NativeKey::Alt
            | NativeKey::AltGr
            | NativeKey::Super
            | NativeKey::PrintScreen => {}
            NativeKey::Char(c) => unsafe {
                let e = CGEventCreateKeyboardEvent(std::ptr::null(), 0, down);
                if !e.is_null() {
                    let mut buf = [0u16; 2];
                    let units = c.encode_utf16(&mut buf);
                    CGEventKeyboardSetUnicodeString(e, units.len(), units.as_ptr());
                }
                post(e, m);
            },
            other => {
                if let Some(code) = mac_keycode(other) {
                    unsafe { post(CGEventCreateKeyboardEvent(std::ptr::null(), code, down), m) };
                }
            }
        }
    }

    pub fn missing_setup() -> Vec<Setup> {
        let mut v = Vec::new();
        if !unsafe { CGPreflightScreenCaptureAccess() } {
            v.push(Setup::ScreenRecording);
        }
        if !unsafe { AXIsProcessTrusted() } {
            v.push(Setup::Accessibility);
        }
        v
    }

    pub fn capture() -> io::Result<Frame> {
        super::super::capture::capture_with_screencapture()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------------------------------------------
#[cfg(target_os = "windows")]
mod sys {
    use super::*;
    use std::ffi::c_void;
    use std::sync::Once;

    #[repr(C)]
    struct MouseInput {
        dx: i32,
        dy: i32,
        data: u32,
        flags: u32,
        time: u32,
        extra: usize,
    }
    #[repr(C)]
    struct KeybdInput {
        vk: u16,
        scan: u16,
        flags: u32,
        time: u32,
        extra: usize,
    }
    #[repr(C)]
    union InputUnion {
        mouse: std::mem::ManuallyDrop<MouseInput>,
        key: std::mem::ManuallyDrop<KeybdInput>,
    }
    #[repr(C)]
    struct Input {
        kind: u32,
        u: InputUnion,
    }
    #[repr(C)]
    struct PointL {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    struct BitmapInfoHeader {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        x_pels: i32,
        y_pels: i32,
        clr_used: u32,
        clr_important: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendInput(count: u32, inputs: *const Input, size: i32) -> u32;
        fn GetCursorPos(p: *mut PointL) -> i32;
        fn SetCursorPos(x: i32, y: i32) -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
        fn SetProcessDPIAware() -> i32;
        fn VkKeyScanW(ch: u16) -> i16;
        fn GetDC(window: *mut c_void) -> *mut c_void;
        fn ReleaseDC(window: *mut c_void, dc: *mut c_void) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: *mut c_void) -> *mut c_void;
        fn CreateCompatibleBitmap(dc: *mut c_void, w: i32, h: i32) -> *mut c_void;
        fn SelectObject(dc: *mut c_void, object: *mut c_void) -> *mut c_void;
        fn BitBlt(
            dst: *mut c_void,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            src: *mut c_void,
            sx: i32,
            sy: i32,
            rop: u32,
        ) -> i32;
        fn GetDIBits(
            dc: *mut c_void,
            bitmap: *mut c_void,
            start: u32,
            lines: u32,
            bits: *mut c_void,
            info: *mut BitmapInfoHeader,
            usage: u32,
        ) -> i32;
        fn DeleteObject(object: *mut c_void) -> i32;
        fn DeleteDC(dc: *mut c_void) -> i32;
    }

    const INPUT_MOUSE: u32 = 0;
    const INPUT_KEYBOARD: u32 = 1;

    /// Without this Windows reports a scaled size and the picture and the pointer disagree.
    fn dpi_aware() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| unsafe {
            SetProcessDPIAware();
        });
    }

    fn send_mouse(flags: u32, data: u32) {
        let input = Input {
            kind: INPUT_MOUSE,
            u: InputUnion {
                mouse: std::mem::ManuallyDrop::new(MouseInput {
                    dx: 0,
                    dy: 0,
                    data,
                    flags,
                    time: 0,
                    extra: 0,
                }),
            },
        };
        unsafe { SendInput(1, &input, std::mem::size_of::<Input>() as i32) };
    }

    fn send_key(vk: u16, scan: u16, flags: u32) {
        let input = Input {
            kind: INPUT_KEYBOARD,
            u: InputUnion {
                key: std::mem::ManuallyDrop::new(KeybdInput {
                    vk,
                    scan,
                    flags,
                    time: 0,
                    extra: 0,
                }),
            },
        };
        unsafe { SendInput(1, &input, std::mem::size_of::<Input>() as i32) };
    }

    pub fn screen_size() -> Option<(u32, u32)> {
        dpi_aware();
        let (w, h) = unsafe { (GetSystemMetrics(0), GetSystemMetrics(1)) };
        (w > 0 && h > 0).then_some((w as u32, h as u32))
    }

    pub fn pointer() -> Option<(u16, u16)> {
        dpi_aware();
        let mut p = PointL { x: 0, y: 0 };
        (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x.max(0) as u16, p.y.max(0) as u16))
    }

    pub fn pointer_to(x: i32, y: i32, _held: Option<Button>, _m: Mods) {
        dpi_aware();
        unsafe { SetCursorPos(x, y) };
    }

    pub fn button(b: Button, down: bool, _clicks: i64, _at: (i32, i32), _m: Mods) {
        let flags = match (b, down) {
            (Button::Left, true) => 0x0002,
            (Button::Left, false) => 0x0004,
            (Button::Right, true) => 0x0008,
            (Button::Right, false) => 0x0010,
            (Button::Middle, true) => 0x0020,
            (Button::Middle, false) => 0x0040,
        };
        send_mouse(flags, 0);
    }

    pub fn wheel(up: bool, _m: Mods) {
        let delta: i32 = if up { 120 } else { -120 };
        send_mouse(0x0800, delta as u32);
    }

    pub fn key(down: bool, key: NativeKey, _m: Mods) {
        let up = if down { 0 } else { 0x0002 };
        match key {
            NativeKey::Char(c) => {
                let mut units = [0u16; 2];
                let units = c.encode_utf16(&mut units);
                // The key a keyboard would press for this character, so shortcuts work; otherwise as text.
                if units.len() == 1 {
                    let scan = unsafe { VkKeyScanW(units[0]) };
                    if scan != -1 {
                        send_key((scan & 0xff) as u16, 0, up);
                        return;
                    }
                }
                for u in units.iter() {
                    send_key(0, *u, 0x0004 | up);
                }
            }
            other => {
                if let Some((vk, extended)) = windows_vk(other) {
                    send_key(vk, 0, up | if extended { 0x0001 } else { 0 });
                }
            }
        }
    }

    pub fn missing_setup() -> Vec<Setup> {
        Vec::new()
    }

    /// The whole screen through GDI, as 32-bit top-down BGRX, which is already the format of `Frame`.
    pub fn capture() -> io::Result<Frame> {
        dpi_aware();
        let (w, h) = screen_size().ok_or_else(|| io::Error::other("no screen"))?;
        let (w, h) = (w as i32, h as i32);
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return Err(io::Error::other("GetDC failed"));
            }
            let mem = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bitmap);
            let copied = BitBlt(mem, 0, 0, w, h, screen, 0, 0, 0x00CC_0020);
            let mut info = BitmapInfoHeader {
                size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                width: w,
                height: -h,
                planes: 1,
                bit_count: 32,
                compression: 0,
                size_image: 0,
                x_pels: 0,
                y_pels: 0,
                clr_used: 0,
                clr_important: 0,
            };
            let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            let lines = if copied != 0 {
                GetDIBits(
                    mem,
                    bitmap,
                    0,
                    h as u32,
                    pixels.as_mut_ptr().cast(),
                    &mut info,
                    0,
                )
            } else {
                0
            };
            SelectObject(mem, old);
            DeleteObject(bitmap);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            if lines == 0 {
                return Err(io::Error::other("screen copy failed"));
            }
            Frame::new(w as u16, h as u16, pixels)
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Linux (and anything else): the command line tools
// ---------------------------------------------------------------------------------------------------------------
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod sys {
    use super::*;
    use std::process::Command;

    fn xdotool(args: &[&str]) {
        let _ = Command::new("xdotool").args(args).status();
    }

    pub fn screen_size() -> Option<(u32, u32)> {
        None
    }

    pub fn pointer() -> Option<(u16, u16)> {
        let out = Command::new("xdotool")
            .args(["getmouselocation", "--shell"])
            .output()
            .ok()?;
        super::parse_mouse_location(&String::from_utf8_lossy(&out.stdout))
    }

    pub fn pointer_to(x: i32, y: i32, _held: Option<Button>, _m: Mods) {
        xdotool(&["mousemove", &x.to_string(), &y.to_string()]);
    }

    pub fn button(b: Button, down: bool, _clicks: i64, _at: (i32, i32), _m: Mods) {
        let n = match b {
            Button::Left => "1",
            Button::Middle => "2",
            Button::Right => "3",
        };
        xdotool(&[if down { "mousedown" } else { "mouseup" }, n]);
    }

    pub fn wheel(up: bool, _m: Mods) {
        xdotool(&["click", if up { "4" } else { "5" }]);
    }

    pub fn key(down: bool, key: NativeKey, _m: Mods) {
        let name = match key {
            NativeKey::Shift => Some("Shift_L"),
            NativeKey::Control => Some("Control_L"),
            NativeKey::Alt => Some("Alt_L"),
            NativeKey::AltGr => Some("ISO_Level3_Shift"),
            NativeKey::Super => Some("Super_L"),
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
            NativeKey::PrintScreen => Some("Print"),
            NativeKey::Function(_) | NativeKey::Char(_) => None,
        };
        let verb = if down { "keydown" } else { "keyup" };
        match (key, name) {
            (_, Some(n)) => xdotool(&[verb, n]),
            (NativeKey::Function(n), _) => xdotool(&[verb, &format!("F{n}")]),
            // `type` presses and releases by itself; with a modifier held it types the shortcut.
            (NativeKey::Char(c), _) if down => {
                xdotool(&["type", "--delay", "0", "--", &c.to_string()]);
            }
            _ => {}
        }
    }

    pub fn missing_setup() -> Vec<Setup> {
        let have = |cmd: &str, arg: &str| {
            Command::new(cmd)
                .arg(arg)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if have("xdotool", "--version") && have("import", "-version") {
            Vec::new()
        } else {
            vec![Setup::LinuxTools]
        }
    }

    pub fn capture() -> io::Result<Frame> {
        super::super::capture::capture_with_import()
    }
}

pub use sys::{button, capture, key, missing_setup, pointer, pointer_to, screen_size, wheel};

/// What the system keeps between captures: a running screen stream on macOS, a connection to the X server on
/// Linux, nothing on Windows.
#[derive(Default)]
pub struct Grabber {
    /// `None`: not tried yet. `Some(None)`: the stream failed, so the `screencapture` command is used.
    #[cfg(target_os = "macos")]
    stream: Option<Option<super::mac_capture::ScreenStream>>,
    /// Same two states for the X server; the fallback is the `import` command.
    #[cfg(all(unix, not(target_os = "macos")))]
    x11: Option<Option<super::x11_capture::X11Screen>>,
}

impl Grabber {
    pub fn grab(&mut self) -> io::Result<Frame> {
        #[cfg(target_os = "macos")]
        {
            let stream = self
                .stream
                .get_or_insert_with(|| super::mac_capture::ScreenStream::start().ok());
            match stream.as_ref().map(|s| s.latest()) {
                Some(Ok(frame)) => return Ok(frame),
                Some(Err(_)) => *stream = None,
                None => {}
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let x11 = self
                .x11
                .get_or_insert_with(|| super::x11_capture::X11Screen::open().ok());
            match x11.as_ref().map(|s| s.grab()) {
                Some(Ok(frame)) => return Ok(frame),
                Some(Err(_)) => *x11 = None,
                None => {}
            }
        }
        capture()
    }
}

/// `xdotool getmouselocation --shell` prints `X=…`, `Y=…`, `SCREEN=…`, `WINDOW=…`.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
pub fn parse_mouse_location(text: &str) -> Option<(u16, u16)> {
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().parse::<u16>().ok())
    };
    Some((get("X=")?, get("Y=")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_scale_to_the_systems_own_units_and_stay_on_screen() {
        // A 1920x1080 screen shared as a 960x540 picture: every point doubles.
        assert_eq!(scale_point(480, 270, (960, 540), (1920, 1080)), (960, 540));
        // Same size: unchanged. The edge never goes past the last pixel.
        assert_eq!(scale_point(10, 20, (1280, 720), (1280, 720)), (10, 20));
        assert_eq!(
            scale_point(959, 539, (960, 540), (1920, 1080)),
            (1918, 1078)
        );
        assert_eq!(scale_point(1279, 719, (1280, 720), (1000, 500)), (999, 499));
        // Unknown sizes pass the point through.
        assert_eq!(scale_point(7, 9, (0, 0), (1920, 1080)), (7, 9));
        assert_eq!(scale_point(7, 9, (1280, 720), (0, 0)), (7, 9));
    }

    #[test]
    fn modifier_keys_are_tracked_and_other_keys_are_not_modifiers() {
        let mut m = Mods::default();
        assert!(m.update(NativeKey::Control, true));
        assert!(m.control && !m.shift);
        assert!(!m.update(NativeKey::Char('c'), true));
        assert!(m.update(NativeKey::Control, false));
        assert!(!m.control);
    }

    #[test]
    fn key_code_tables_cover_the_editing_keys_and_never_a_character() {
        for k in [
            NativeKey::Enter,
            NativeKey::Backspace,
            NativeKey::ArrowLeft,
            NativeKey::Function(5),
        ] {
            assert!(mac_keycode(k).is_some(), "{k:?}");
            assert!(windows_vk(k).is_some(), "{k:?}");
        }
        assert_eq!(mac_keycode(NativeKey::Enter), Some(36));
        assert_eq!(windows_vk(NativeKey::Function(1)), Some((0x70, false)));
        assert_eq!(windows_vk(NativeKey::Delete), Some((0x2e, true)));
        assert_eq!(windows_vk(NativeKey::Char('a')), None);
        assert_eq!(mac_keycode(NativeKey::Char('a')), None);
        assert_eq!(mac_keycode(NativeKey::Function(13)), None);
    }

    /// Run by hand on a real desktop: `cargo test live_os -- --ignored --nocapture`. It only "moves" the cursor to
    /// where it already is, never clicks or types.
    #[test]
    #[ignore = "talks to the real desktop"]
    fn live_os_pointer_size_capture_and_setup() {
        let here = pointer().expect("cursor position");
        pointer_to(i32::from(here.0), i32::from(here.1), None, Mods::default());
        std::thread::sleep(std::time::Duration::from_millis(50));
        let after = pointer().expect("cursor position again");
        println!("cursor {here:?} -> {after:?}, screen {:?}", screen_size());
        assert!(here.0.abs_diff(after.0) <= 1 && here.1.abs_diff(after.1) <= 1);
        if let Some((w, h)) = screen_size() {
            assert!(w > 0 && h > 0);
        }
        // A real move: 40 units away and back, which shows the system accepts our events (macOS needs the
        // Accessibility permission for this).
        let (tx, ty) = (i32::from(here.0) + 40, i32::from(here.1) + 40);
        pointer_to(tx, ty, None, Mods::default());
        std::thread::sleep(std::time::Duration::from_millis(80));
        let moved = pointer().expect("cursor position after the move");
        pointer_to(i32::from(here.0), i32::from(here.1), None, Mods::default());
        println!("moved to {moved:?}, wanted ({tx}, {ty})");
        assert!(i32::from(moved.0).abs_diff(tx) <= 1 && i32::from(moved.1).abs_diff(ty) <= 1);
        let frame = capture().expect("a frame of the screen");
        println!(
            "frame {}x{}, missing setup {:?}",
            frame.width,
            frame.height,
            missing_setup()
        );
        assert!(frame.width > 0 && frame.height > 0);
    }

    #[test]
    fn the_xdotool_location_output_is_parsed() {
        assert_eq!(
            parse_mouse_location("X=640\nY=360\nSCREEN=0\nWINDOW=4194307\n"),
            Some((640, 360))
        );
        assert_eq!(parse_mouse_location("nothing"), None);
        assert_eq!(parse_mouse_location("X=-5\nY=3\n"), None);
    }
}
