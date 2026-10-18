//! X11 keysyms, the key vocabulary of RFB.

use eframe::egui::Key;

pub const SHIFT: u32 = 0xffe1;
pub const CONTROL: u32 = 0xffe3;
pub const ALT: u32 = 0xffe9;
pub const ALT_GR: u32 = 0xfe03;
pub const DELETE: u32 = 0xffff;
pub const ESCAPE: u32 = 0xff1b;
pub const TAB: u32 = 0xff09;
pub const F4: u32 = 0xffc1;
pub const PRINT: u32 = 0xff61;
pub const SUPER: u32 = 0xffeb;

pub fn from_char(c: char) -> u32 {
    match c as u32 {
        cp @ 0x20..=0x7e | cp @ 0xa0..=0xff => cp,
        cp => 0x0100_0000 | cp,
    }
}

pub fn from_special_key(key: Key) -> Option<u32> {
    Some(match key {
        Key::Backspace => 0xff08,
        Key::Tab => 0xff09,
        Key::Enter => 0xff0d,
        Key::Escape => 0xff1b,
        Key::Delete => DELETE,
        Key::Insert => 0xff63,
        Key::Home => 0xff50,
        Key::ArrowLeft => 0xff51,
        Key::ArrowUp => 0xff52,
        Key::ArrowRight => 0xff53,
        Key::ArrowDown => 0xff54,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::End => 0xff57,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
        _ => return None,
    })
}
