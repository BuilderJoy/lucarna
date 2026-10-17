//! KeyEvent and PointerEvent (RFC 6143 §7.5.4–7.5.5).

use std::io;

use super::keysym::{ALT, CONTROL, DELETE};
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

/// Windows' secure attention sequence, which the local OS would otherwise intercept.
pub fn ctrl_alt_del(out: &Outbound) -> io::Result<()> {
    for (down, sym) in [(true, CONTROL), (true, ALT), (true, DELETE), (false, DELETE), (false, ALT), (false, CONTROL)] {
        key(out, down, sym)?;
    }
    Ok(())
}
