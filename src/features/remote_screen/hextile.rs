//! Hextile encoding (RFC 6143 §7.7.4): 16×16 tiles of a background plus solid subrectangles.

use std::io::{self, Read};

use crate::shared::wire::{invalid, read_bytes, read_u8};

const TILE: usize = 16;
const RAW: u8 = 1;
const BACKGROUND: u8 = 2;
const FOREGROUND: u8 = 4;
const ANY_SUBRECTS: u8 = 8;
const SUBRECTS_COLOURED: u8 = 16;

/// Reads a w×h Hextile rectangle as BGRX pixels.
pub fn read(s: &mut impl Read, w: usize, h: usize) -> io::Result<Vec<u8>> {
    let mut out = vec![0; w * h * 4];
    // Background and foreground carry over from one tile to the next.
    let (mut bg, mut fg) = ([0; 4], [0; 4]);
    for ty in (0..h).step_by(TILE) {
        for tx in (0..w).step_by(TILE) {
            let (tw, th) = (TILE.min(w - tx), TILE.min(h - ty));
            let mut fill = |x: usize, y: usize, cw: usize, ch: usize, p: &[u8]| {
                for r in 0..ch {
                    for c in 0..cw {
                        let at = ((ty + y + r) * w + tx + x + c) * 4;
                        out[at..at + 4].copy_from_slice(&p[(r * cw + c) * 4..][..4]);
                    }
                }
            };
            let flags = read_u8(s)?;
            if flags & RAW != 0 {
                fill(0, 0, tw, th, &read_bytes(s, tw * th * 4)?);
                continue;
            }
            if flags & BACKGROUND != 0 {
                bg = pixel(s)?;
            }
            fill(0, 0, tw, th, &bg.repeat(tw * th));
            if flags & FOREGROUND != 0 {
                fg = pixel(s)?;
            }
            if flags & ANY_SUBRECTS == 0 {
                continue;
            }
            let n = read_u8(s)? as usize;
            let coloured = flags & SUBRECTS_COLOURED != 0;
            let sub = read_bytes(s, n * if coloured { 6 } else { 2 })?;
            for r in sub.chunks_exact(if coloured { 6 } else { 2 }) {
                let (colour, xy) = if coloured { (&r[..4], &r[4..]) } else { (&fg[..], r) };
                let (x, y) = ((xy[0] >> 4) as usize, (xy[0] & 15) as usize);
                let (sw, sh) = ((xy[1] >> 4) as usize + 1, (xy[1] & 15) as usize + 1);
                if x + sw > tw || y + sh > th {
                    return Err(invalid("Hextile subrect outside tile"));
                }
                fill(x, y, sw, sh, &colour.repeat(sw * sh));
            }
        }
    }
    Ok(out)
}

fn pixel(s: &mut impl Read) -> io::Result<[u8; 4]> {
    Ok(read_bytes(s, 4)?.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 4] = [1, 1, 1, 0];
    const B: [u8; 4] = [2, 2, 2, 0];

    fn pixels(mut data: &[u8], w: usize, h: usize) -> io::Result<Vec<[u8; 4]>> {
        let px = read(&mut data, w, h)?.chunks_exact(4).map(|p| p.try_into().unwrap()).collect();
        assert!(data.is_empty(), "unread bytes");
        Ok(px)
    }

    #[test]
    fn tiles() {
        // Background A, foreground B, one 1×2 subrect at (1,0).
        let t = [&[BACKGROUND | FOREGROUND | ANY_SUBRECTS][..], &A, &B, &[1, 0x10, 0x01]].concat();
        assert_eq!(pixels(&t, 2, 2).unwrap(), [A, B, A, B]);
        // Coloured subrect at (0,1), 1×1.
        let t = [&[BACKGROUND | ANY_SUBRECTS | SUBRECTS_COLOURED][..], &A, &[1], &B, &[0x01, 0x00]].concat();
        assert_eq!(pixels(&t, 1, 2).unwrap(), [A, B]);
        // Raw tile.
        assert_eq!(pixels(&[&[RAW][..], &B, &A].concat(), 2, 1).unwrap(), [B, A]);
    }

    #[test]
    fn background_carries_to_next_tile() {
        // 17×1 = two tiles; the second has no flags and reuses B.
        let px = pixels(&[&[BACKGROUND][..], &B, &[0]].concat(), 17, 1).unwrap();
        assert!(px.iter().all(|&p| p == B));
    }

    #[test]
    fn rejects_subrect_outside_tile() {
        let t = [&[BACKGROUND | FOREGROUND | ANY_SUBRECTS][..], &A, &B, &[1, 0x10, 0x10]].concat();
        assert!(pixels(&t, 2, 1).is_err());
    }
}
