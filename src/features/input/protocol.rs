//! KeyEvent and PointerEvent (RFC 6143 §7.5.4–7.5.5).

use std::io;

use super::keysym::{ALT, CONTROL, DELETE, ESCAPE, F4, PRINT, SUPER, TAB};
use crate::session::Outbound;

pub fn key(out: &Outbound, down: bool, keysym: u32) -> io::Result<()> {
    let mut m = vec![4, down as u8, 0, 0];
    m.extend_from_slice(&keysym.to_be_bytes());
    out.send(&m)
}

pub fn tap(out: &Outbound, keysym: u32) -> io::Result<()> {
    key(out, true, keysym)?;
    key(out, false, keysym)
}

pub fn pointer(out: &Outbound, buttons: u8, (x, y): (u16, u16)) -> io::Result<()> {
    let mut m = vec![5, buttons];
    m.extend_from_slice(&x.to_be_bytes());
    m.extend_from_slice(&y.to_be_bytes());
    out.send(&m)
}

/// Key combinations the local OS would otherwise intercept, for the session bar's Keys menu.
pub const SHORTCUTS: &[(&str, &[u32])] = &[
    ("Ctrl+Esc", &[CONTROL, ESCAPE]),
    ("Alt+Tab", &[ALT, TAB]),
    ("Alt+F4", &[ALT, F4]),
    ("Windows key", &[SUPER]),
    ("Print Screen", &[PRINT]),
];

/// Windows' secure attention sequence, which the local OS would otherwise intercept.
pub fn ctrl_alt_del(out: &Outbound) -> io::Result<()> {
    combo(out, &[CONTROL, ALT, DELETE])
}

/// Presses the keys in order, then releases them in reverse.
pub fn combo(out: &Outbound, keys: &[u32]) -> io::Result<()> {
    combo_events(keys).try_for_each(|(down, sym)| key(out, down, sym))
}

fn combo_events(keys: &[u32]) -> impl Iterator<Item = (bool, u32)> + '_ {
    keys.iter().map(|&k| (true, k)).chain(keys.iter().rev().map(|&k| (false, k)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_releases_in_reverse() {
        assert_eq!(combo_events(&[CONTROL, ALT, DELETE]).collect::<Vec<_>>(), [(true, CONTROL), (true, ALT), (true, DELETE), (false, DELETE), (false, ALT), (false, CONTROL)]);
    }
}
