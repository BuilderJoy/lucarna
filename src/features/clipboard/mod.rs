//! Clipboard: ServerCutText / ClientCutText (RFC 6143 §7.5.6, §7.6.4) and the
//! Extended Clipboard pseudo-encoding (`0xC0A1E5CE`) for zlib-compressed UTF-8.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Event};
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::session::Outbound;
use crate::shared::wire::{invalid, read_bytes, read_u32};

/// Pseudo-encoding that would negotiate Extended Clipboard (UTF-8). Not advertised: libvncserver
/// 0.9.14 (x11vnc 0.9.16) closes the connection on our UTF-8 Provide. Text stays Latin-1.
/// ponytail: re-advertise once a server that handles UTF-8 Provide is verified (UltraVNC).
#[allow(dead_code)]
pub const ENC_EXTENDED_CLIPBOARD: i32 = 0xC0A1_E5CE_u32 as i32;

const MAX_TEXT: usize = 16 << 20;

// Format flags (lower 16 bits).
const CLIP_UTF8: u32 = 1 << 0;

// Action flags (bits 24..31).
const CLIP_CAPS: u32 = 1 << 24;
const CLIP_REQUEST: u32 = 1 << 25;
const CLIP_PEEK: u32 = 1 << 26;
const CLIP_NOTIFY: u32 = 1 << 27;
const CLIP_PROVIDE: u32 = 1 << 28;

/// Session-scoped clipboard handler that negotiates Extended Clipboard (UTF-8) when the
/// server supports it and falls back to classic RFB Latin-1 otherwise.
pub struct RemoteClipboard {
    outbound: Arc<Outbound>,
    state: ProtocolState,
}

#[derive(Default)]
struct ProtocolState {
    extended: AtomicBool,
    last_sent: Mutex<String>,
}

impl RemoteClipboard {
    pub fn new(outbound: Arc<Outbound>) -> Self {
        Self {
            outbound,
            state: ProtocolState::default(),
        }
    }

    /// Reads a `ServerCutText` body (after message-type byte `3`), answers Extended Clipboard
    /// control messages (`Caps`, `Notify`, `Request`, `Peek`), and returns `Some(text)` when
    /// new clipboard text arrives from the server.
    pub fn read_server_msg(&self, s: &mut impl Read) -> io::Result<Option<String>> {
        let mut reply = Vec::new();
        let text = self.state.process_server_msg(s, &mut reply)?;
        if !reply.is_empty() {
            self.outbound.send(&reply)?;
        }
        Ok(text)
    }

    /// Sends clipboard text to the server using Extended Clipboard UTF-8 if negotiated,
    /// or classic Latin-1 `ClientCutText` as fallback.
    pub fn send_client_text(&self, text: &str) -> io::Result<()> {
        let pkt = self.state.encode_client_msg(text)?;
        self.outbound.send(&pkt)
    }

    /// On local paste, hands the text to the server. Must run before input forwards Ctrl+V.
    pub fn forward_paste(&self, ui: &egui::Ui) {
        ui.input(|i| {
            for event in &i.events {
                if let Event::Paste(text) = event {
                    let _ = self.send_client_text(text);
                }
            }
        });
    }
}

impl ProtocolState {
    fn process_server_msg(&self, s: &mut impl Read, reply: &mut Vec<u8>) -> io::Result<Option<String>> {
        read_bytes(s, 3)?;
        let raw_len = read_u32(s)? as i32;
        if raw_len >= 0 {
            let len = raw_len as usize;
            if len > MAX_TEXT {
                return Err(invalid("clipboard too large"));
            }
            let text: String = read_bytes(s, len)?.into_iter().map(char::from).collect();
            return Ok(Some(normalize_lf(&text)));
        }

        let len = raw_len.unsigned_abs() as usize;
        if !(4..=MAX_TEXT).contains(&len) {
            return Err(invalid("invalid extended clipboard length"));
        }
        let flags = read_u32(s)?;
        let payload = read_bytes(s, len - 4)?;
        self.extended.store(true, Ordering::Relaxed);

        if flags & CLIP_CAPS != 0 {
            *reply = encode_caps();
            return Ok(None);
        }
        if flags & CLIP_PROVIDE != 0 {
            if flags & CLIP_UTF8 != 0 {
                return decode_utf8_provide(&payload).map(Some);
            }
            return Ok(None);
        }
        if flags & CLIP_NOTIFY != 0 {
            if flags & CLIP_UTF8 != 0 {
                *reply = encode_action(CLIP_REQUEST | CLIP_UTF8);
            }
            return Ok(None);
        }
        if flags & CLIP_REQUEST != 0 {
            if flags & CLIP_UTF8 != 0 {
                let text = self.last_sent.lock().unwrap().clone();
                if !text.is_empty() {
                    *reply = encode_utf8_provide(&text)?;
                }
            }
            return Ok(None);
        }
        if flags & CLIP_PEEK != 0 {
            if !self.last_sent.lock().unwrap().is_empty() {
                *reply = encode_action(CLIP_NOTIFY | CLIP_UTF8);
            }
            return Ok(None);
        }
        Ok(None)
    }

    fn encode_client_msg(&self, text: &str) -> io::Result<Vec<u8>> {
        *self.last_sent.lock().unwrap() = text.to_string();
        if self.extended.load(Ordering::Relaxed) {
            encode_utf8_provide(text)
        } else {
            Ok(encode_latin1(text))
        }
    }
}

fn encode_latin1(text: &str) -> Vec<u8> {
    let bytes: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
    let mut m = vec![6, 0, 0, 0];
    m.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    m.extend_from_slice(&bytes);
    m
}

fn encode_caps() -> Vec<u8> {
    let flags = CLIP_CAPS | CLIP_UTF8 | CLIP_REQUEST | CLIP_PEEK | CLIP_NOTIFY | CLIP_PROVIDE;
    let mut m = vec![6, 0, 0, 0];
    m.extend_from_slice(&(-8_i32).to_be_bytes());
    m.extend_from_slice(&flags.to_be_bytes());
    m.extend_from_slice(&(MAX_TEXT as u32).to_be_bytes());
    m
}

fn encode_action(flags: u32) -> Vec<u8> {
    let mut m = vec![6, 0, 0, 0];
    m.extend_from_slice(&(-4_i32).to_be_bytes());
    m.extend_from_slice(&flags.to_be_bytes());
    m
}

fn encode_utf8_provide(text: &str) -> io::Result<Vec<u8>> {
    // Extended Clipboard specifies CRLF line endings and a trailing NUL byte for UTF-8 text.
    let crlf = normalize_crlf(text);
    let mut raw = Vec::with_capacity(4 + crlf.len() + 1);
    let byte_len = (crlf.len() + 1) as u32;
    raw.extend_from_slice(&byte_len.to_be_bytes());
    raw.extend_from_slice(crlf.as_bytes());
    raw.push(0);

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&raw)?;
    let compressed = enc.finish()?;

    let ext_len = -((4 + compressed.len()) as i32);
    let mut m = Vec::with_capacity(8 + 4 + compressed.len());
    m.extend_from_slice(&[6, 0, 0, 0]);
    m.extend_from_slice(&ext_len.to_be_bytes());
    m.extend_from_slice(&(CLIP_PROVIDE | CLIP_UTF8).to_be_bytes());
    m.extend_from_slice(&compressed);
    Ok(m)
}

fn decode_utf8_provide(compressed: &[u8]) -> io::Result<String> {
    let mut dec = ZlibDecoder::new(compressed).take((MAX_TEXT + 4) as u64);
    let size = read_u32(&mut dec)? as usize;
    if size > MAX_TEXT {
        return Err(invalid("extended clipboard UTF-8 payload too large"));
    }
    let mut bytes = read_bytes(&mut dec, size)?;
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    let text = String::from_utf8(bytes).map_err(|_| invalid("extended clipboard is not valid UTF-8"))?;
    Ok(normalize_lf(&text))
}

fn normalize_lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn normalize_crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push_str("\r\n");
        } else if c == '\n' {
            out.push_str("\r\n");
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn latin1_fallback_when_extended_not_negotiated() {
        let mut incoming = vec![0, 0, 0];
        let body = b"Caf\xe9\r\n";
        incoming.extend_from_slice(&(body.len() as u32).to_be_bytes());
        incoming.extend_from_slice(body);

        let cb = ProtocolState::default();
        let mut reply = Vec::new();
        assert_eq!(cb.process_server_msg(&mut Cursor::new(incoming), &mut reply).unwrap().as_deref(), Some("Café\n"));
        assert!(reply.is_empty());

        let sent = cb.encode_client_msg("Ñandú 🚀").unwrap();
        assert_eq!(&sent[..4], &[6, 0, 0, 0]);
        let len = u32::from_be_bytes(sent[4..8].try_into().unwrap()) as usize;
        assert_eq!(&sent[8..8 + len], b"\xd1and\xfa ?");
    }

    #[test]
    fn extended_caps_and_utf8_provide_round_trip() {
        let cb = ProtocolState::default();

        // Server announces Extended Clipboard capabilities.
        let mut caps_msg = vec![0, 0, 0];
        caps_msg.extend_from_slice(&(-8_i32).to_be_bytes());
        caps_msg.extend_from_slice(&(CLIP_CAPS | CLIP_UTF8 | CLIP_PROVIDE | CLIP_NOTIFY).to_be_bytes());
        caps_msg.extend_from_slice(&(1024 * 1024_u32).to_be_bytes());
        let mut reply = Vec::new();
        assert_eq!(cb.process_server_msg(&mut Cursor::new(caps_msg), &mut reply).unwrap(), None);

        // Client replies with its own CLIP_CAPS message.
        assert_eq!(&reply[..4], &[6, 0, 0, 0]);
        assert_eq!(i32::from_be_bytes(reply[4..8].try_into().unwrap()), -8);
        let reply_flags = u32::from_be_bytes(reply[8..12].try_into().unwrap());
        assert!(reply_flags & CLIP_CAPS != 0 && reply_flags & CLIP_UTF8 != 0);

        // Client now encodes UTF-8 with emojis and typographic symbols via CLIP_PROVIDE.
        let sample = "¡Hola España! — “UTF-8” 🚀\nSegunda línea";
        let pkt = cb.encode_client_msg(sample).unwrap();
        assert_eq!(pkt[0], 6);

        let mut reply2 = Vec::new();
        let received = cb.process_server_msg(&mut Cursor::new(&pkt[1..]), &mut reply2).unwrap();
        assert_eq!(received.as_deref(), Some(sample));
    }

    #[test]
    fn notify_requests_utf8_and_request_provides_cached_text() {
        let cb = ProtocolState {
            extended: AtomicBool::new(true),
            last_sent: Mutex::new("Guardado".into()),
        };

        // Server sends CLIP_NOTIFY | CLIP_UTF8 -> client replies with CLIP_REQUEST | CLIP_UTF8.
        let mut notify = vec![0, 0, 0];
        notify.extend_from_slice(&(-4_i32).to_be_bytes());
        notify.extend_from_slice(&(CLIP_NOTIFY | CLIP_UTF8).to_be_bytes());
        let mut reply = Vec::new();
        assert_eq!(cb.process_server_msg(&mut Cursor::new(notify), &mut reply).unwrap(), None);
        assert_eq!(&reply[..4], &[6, 0, 0, 0]);
        assert_eq!(i32::from_be_bytes(reply[4..8].try_into().unwrap()), -4);
        assert_eq!(u32::from_be_bytes(reply[8..12].try_into().unwrap()), CLIP_REQUEST | CLIP_UTF8);

        // Server sends CLIP_REQUEST | CLIP_UTF8 -> client replies with CLIP_PROVIDE | CLIP_UTF8.
        let mut req = vec![0, 0, 0];
        req.extend_from_slice(&(-4_i32).to_be_bytes());
        req.extend_from_slice(&(CLIP_REQUEST | CLIP_UTF8).to_be_bytes());
        reply.clear();
        assert_eq!(cb.process_server_msg(&mut Cursor::new(req), &mut reply).unwrap(), None);
        let mut dummy = Vec::new();
        assert_eq!(
            cb.process_server_msg(&mut Cursor::new(&reply[1..]), &mut dummy).unwrap().as_deref(),
            Some("Guardado")
        );
    }
}
