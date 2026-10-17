//! Pixel format, encodings and FramebufferUpdate (RFC 6143 §7.5.1–7.5.3, §7.6.1).

use std::io::{self, Read, Write};
use std::sync::Mutex;

use flate2::Decompress;

use super::{hextile, zrle, Framebuffer};
use crate::session::Outbound;
use crate::shared::wire::{invalid, read_bytes, read_u16, read_u32, read_u8};

const ENC_RAW: i32 = 0;
const ENC_COPY_RECT: i32 = 1;
const ENC_HEXTILE: i32 = 5;
const ENC_ZLIB: i32 = 6;
const ENC_ZRLE: i32 = 16;
const ENC_LAST_RECT: i32 = -224;
const ENC_DESKTOP_SIZE: i32 = -223;

/// Asks for BGRX true colour and the encodings we can decode.
pub fn negotiate(s: &mut impl Write) -> io::Result<()> {
    #[rustfmt::skip]
    s.write_all(&[0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0])?;
    let encodings = [ENC_ZRLE, ENC_ZLIB, ENC_HEXTILE, ENC_COPY_RECT, ENC_RAW, ENC_LAST_RECT, ENC_DESKTOP_SIZE];
    let mut m = vec![2, 0];
    m.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for e in encodings {
        m.extend_from_slice(&e.to_be_bytes());
    }
    s.write_all(&m)
}

pub fn request_update(out: &Outbound, incremental: bool, screen: &Mutex<Framebuffer>) -> io::Result<()> {
    let (w, h) = screen.lock().unwrap().size();
    let mut m = vec![3, incremental as u8, 0, 0, 0, 0];
    m.extend_from_slice(&w.to_be_bytes());
    m.extend_from_slice(&h.to_be_bytes());
    out.send(&m)
}

/// Decompression state that lives as long as the session (zlib streams span updates).
pub struct Decoder {
    zrle: Decompress,
    zlib: Decompress,
}

impl Default for Decoder {
    fn default() -> Self {
        Self { zrle: Decompress::new(true), zlib: Decompress::new(true) }
    }
}

/// Reads a u32-length zlib block and inflates it from `z`, refusing more than `max` bytes of output.
fn read_zlib(s: &mut impl Read, z: &mut Decompress, max: usize) -> io::Result<Vec<u8>> {
    let len = read_u32(s)? as usize;
    if len > zrle::MAX_COMPRESSED {
        return Err(invalid("zlib data too large"));
    }
    zrle::inflate(z, &read_bytes(s, len)?, max)
}

/// Reads a FramebufferUpdate body (after the message-type byte) into `screen`.
pub fn read_update(s: &mut impl Read, screen: &Mutex<Framebuffer>, decoder: &mut Decoder) -> io::Result<()> {
    read_u8(s)?;
    // With LastRect, servers may send 0xFFFF and end the update with a LastRect rectangle.
    let rects = read_u16(s)?;
    for _ in 0..rects {
        let [x, y, w, h] = [0; 4].map(|_| read_u16(s).map(usize::from));
        let (x, y, w, h) = (x?, y?, w?, h?);
        match read_u32(s)? as i32 {
            ENC_DESKTOP_SIZE => *screen.lock().unwrap() = Framebuffer::new(w, h)?,
            ENC_LAST_RECT => break,
            _ if !screen.lock().unwrap().contains(x, y, w, h) => return Err(invalid("rectangle outside screen")),
            ENC_RAW => {
                let data = read_bytes(s, w * h * 4)?;
                screen.lock().unwrap().put(x, y, w, h, &data);
            }
            ENC_COPY_RECT => {
                let (sx, sy) = (read_u16(s)? as usize, read_u16(s)? as usize);
                let mut fb = screen.lock().unwrap();
                if !fb.contains(sx, sy, w, h) {
                    return Err(invalid("copy source outside screen"));
                }
                let data = fb.get(sx, sy, w, h);
                fb.put(x, y, w, h, &data);
            }
            ENC_ZRLE => {
                let data = read_zlib(s, &mut decoder.zrle, zrle::max_inflated(w, h))?;
                let pixels = zrle::decode(&data, w, h)?;
                screen.lock().unwrap().put(x, y, w, h, &pixels);
            }
            ENC_ZLIB => {
                let pixels = read_zlib(s, &mut decoder.zlib, w * h * 4)?;
                if pixels.len() != w * h * 4 {
                    return Err(invalid("zlib rectangle has wrong size"));
                }
                screen.lock().unwrap().put(x, y, w, h, &pixels);
            }
            ENC_HEXTILE => {
                let pixels = hextile::read(s, w, h)?;
                screen.lock().unwrap().put(x, y, w, h, &pixels);
            }
            e => return Err(invalid(format!("unsupported encoding {e}"))),
        }
    }
    Ok(())
}

/// Colour maps are unused in true-colour mode.
pub fn skip_colour_map(s: &mut impl Read) -> io::Result<()> {
    read_bytes(s, 3)?;
    let n = read_u16(s)? as usize;
    read_bytes(s, n * 6).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_rect_ends_open_ended_update() {
        let screen = Mutex::new(Framebuffer::new(1, 1).unwrap());
        let mut msg = vec![0, 0xFF, 0xFF];
        msg.extend([0, 0, 0, 0, 0, 1, 0, 1].iter().chain(&ENC_RAW.to_be_bytes()).chain(&[7, 7, 7, 0]));
        msg.extend([0; 8].iter().chain(&ENC_LAST_RECT.to_be_bytes()));
        read_update(&mut &msg[..], &screen, &mut Decoder::default()).unwrap();
        assert_eq!(screen.lock().unwrap().pixels, [7, 7, 7, 0]);
    }
}
