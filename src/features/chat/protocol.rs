//! UltraVNC TextChat (RFB message type 11), both directions: `pad(3) length(u32)` then `length` bytes of text.
//! The length doubles as a command: Open, Close or Finished. Text is Windows-ANSI, read as Latin-1.

use std::io::{self, Read};

use crate::session::Outbound;
use crate::shared::wire::{from_latin1, invalid, read_bytes, read_u32, to_latin1};

const OPEN: u32 = 0xFFFF_FFFF;
const CLOSE: u32 = 0xFFFF_FFFE;
const FINISHED: u32 = 0xFFFF_FFFD;
/// The official viewer's buffer size.
pub const MAX_TEXT: usize = 16384;

#[derive(Debug, PartialEq)]
pub enum Msg {
    Open,
    Close,
    Finished,
    Text(String),
}

/// Reads one TextChat message body (after the type byte).
pub fn read(s: &mut impl Read) -> io::Result<Msg> {
    read_bytes(s, 3)?;
    Ok(match read_u32(s)? {
        OPEN => Msg::Open,
        CLOSE => Msg::Close,
        FINISHED => Msg::Finished,
        n if n as usize > MAX_TEXT => return Err(invalid("chat message too large")),
        n => Msg::Text(from_latin1(&read_bytes(s, n as usize)?)),
    })
}

fn message(length: u32, text: &[u8]) -> Vec<u8> {
    let mut m = vec![11, 0, 0, 0];
    m.extend_from_slice(&length.to_be_bytes());
    m.extend_from_slice(text);
    m
}

// Send errors surface through the session reader as a Closed event, so they are ignored here.
pub fn open(out: &Outbound) {
    let _ = out.send(&message(OPEN, &[]));
}

pub fn close(out: &Outbound) {
    let _ = out.send(&message(CLOSE, &[]));
}

/// The official viewer ends every line with a newline, which the server's chat window shows as a line break.
pub fn say(out: &Outbound, line: &str) {
    let bytes = to_latin1(&format!("{line}\n"));
    if bytes.len() <= MAX_TEXT {
        let _ = out.send(&message(bytes.len() as u32, &bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_commands_and_latin1_text() {
        let read = |m: &[u8]| read(&mut &m[1..]);
        assert_eq!(read(&message(OPEN, &[])).unwrap(), Msg::Open);
        assert_eq!(read(&message(CLOSE, &[])).unwrap(), Msg::Close);
        assert_eq!(read(&message(FINISHED, &[])).unwrap(), Msg::Finished);
        assert_eq!(read(&message(5, b"ma\xf1an")).unwrap(), Msg::Text("mañan".into()));
        assert_eq!(read(&message(0, &[])).unwrap(), Msg::Text(String::new()));
    }

    #[test]
    fn refuses_oversized_text_before_allocating() {
        assert!(read(&mut &message(MAX_TEXT as u32 + 1, &[])[1..]).is_err());
    }

    #[test]
    fn wire_layout() {
        assert_eq!(message(3, b"hi\n"), [11, 0, 0, 0, 0, 0, 0, 3, b'h', b'i', b'\n']);
        assert_eq!(&message(OPEN, &[])[4..], [0xFF; 4]);
    }
}
