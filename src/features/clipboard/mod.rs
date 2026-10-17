//! Clipboard: ServerCutText / ClientCutText (RFC 6143 §7.5.6, §7.6.4). RFB text is Latin-1.

use std::io::{self, Read};

use eframe::egui::{self, Event};

use crate::session::Outbound;
use crate::shared::wire::{invalid, read_bytes, read_u32};

const MAX_TEXT: usize = 16 << 20;

/// Reads a ServerCutText body (after the message-type byte).
pub fn read_server_text(s: &mut impl Read) -> io::Result<String> {
    read_bytes(s, 3)?;
    let len = read_u32(s)? as usize;
    if len > MAX_TEXT {
        return Err(invalid("clipboard too large"));
    }
    Ok(read_bytes(s, len)?.into_iter().map(char::from).collect())
}

/// Characters outside Latin-1 become '?'.
pub fn send_client_text(out: &Outbound, text: &str) -> io::Result<()> {
    let bytes: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
    let mut m = vec![6, 0, 0, 0];
    m.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    m.extend_from_slice(&bytes);
    out.send(&m)
}

/// On local paste, hands the text to the server. Must run before input forwards Ctrl+V.
pub fn forward_paste(ui: &egui::Ui, out: &Outbound) {
    ui.input(|i| {
        for event in &i.events {
            if let Event::Paste(text) = event {
                let _ = send_client_text(out, text);
            }
        }
    });
}
