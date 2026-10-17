//! ZRLE encoding (RFC 6143 §7.7.6): 64×64 tiles inside a session-long zlib stream.

use std::io;

use flate2::{Decompress, FlushDecompress};

use crate::shared::wire::invalid;

pub const MAX_COMPRESSED: usize = 64 << 20;
const TILE: usize = 64;

type Pixel = [u8; 4];

/// Inflates one rectangle (ZRLE or Zlib)'s data from the shared stream, refusing more than `max` bytes of output.
pub fn inflate(z: &mut Decompress, input: &[u8], max: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        out.reserve(64 << 10);
        let (before_in, before_out) = (z.total_in(), z.total_out());
        z.decompress_vec(&input[pos..], &mut out, FlushDecompress::Sync).map_err(|e| invalid(e.to_string()))?;
        pos += (z.total_in() - before_in) as usize;
        if out.len() > max {
            return Err(invalid("zlib data too large"));
        }
        if pos == input.len() && out.len() < out.capacity() {
            return Ok(out);
        }
        if (z.total_in(), z.total_out()) == (before_in, before_out) {
            return Err(invalid("zlib stream stalled"));
        }
    }
}

/// Largest valid inflated size for a w×h rectangle: 4 bytes/pixel (plain RLE runs of 1) plus
/// a subencoding byte and a 127-colour palette per tile.
pub fn max_inflated(w: usize, h: usize) -> usize {
    w * h * 4 + w.div_ceil(TILE) * h.div_ceil(TILE) * (1 + 127 * 3)
}

/// Decodes inflated ZRLE data into w×h BGRX pixels.
pub fn decode(mut s: &[u8], w: usize, h: usize) -> io::Result<Vec<u8>> {
    let mut out = vec![0; w * h * 4];
    for ty in (0..h).step_by(TILE) {
        for tx in (0..w).step_by(TILE) {
            let tw = TILE.min(w - tx);
            for (i, p) in tile(&mut s, tw, TILE.min(h - ty))?.iter().enumerate() {
                let at = ((ty + i / tw) * w + tx + i % tw) * 4;
                out[at..at + 4].copy_from_slice(p);
            }
        }
    }
    if !s.is_empty() {
        return Err(invalid("trailing ZRLE data"));
    }
    Ok(out)
}

fn tile(s: &mut &[u8], w: usize, h: usize) -> io::Result<Vec<Pixel>> {
    let n = w * h;
    let sub = take(s, 1)?[0] as usize;
    match sub {
        0 => (0..n).map(|_| cpixel(s)).collect(),
        1 => Ok(vec![cpixel(s)?; n]),
        2..=16 => {
            let pal = palette(s, sub)?;
            let bits = match sub {
                2 => 1,
                3 | 4 => 2,
                _ => 4,
            };
            let mut px = Vec::with_capacity(n);
            for _ in 0..h {
                let row = take(s, (w * bits).div_ceil(8))?;
                for x in 0..w {
                    let b = x * bits;
                    let i = (row[b / 8] >> (8 - bits - b % 8)) & ((1 << bits) - 1);
                    px.push(lookup(&pal, i)?);
                }
            }
            Ok(px)
        }
        128 | 130..=255 => {
            let pal = if sub == 128 { None } else { Some(palette(s, sub - 128)?) };
            let mut px = Vec::with_capacity(n);
            while px.len() < n {
                let (colour, run) = match &pal {
                    None => (cpixel(s)?, run_length(s)?),
                    Some(pal) => {
                        let i = take(s, 1)?[0];
                        (lookup(pal, i & 127)?, if i & 128 != 0 { run_length(s)? } else { 1 })
                    }
                };
                if px.len() + run > n {
                    return Err(invalid("ZRLE run overflows tile"));
                }
                px.resize(px.len() + run, colour);
            }
            Ok(px)
        }
        _ => Err(invalid(format!("invalid ZRLE subencoding {sub}"))),
    }
}

fn take<'a>(s: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    let (head, rest) = s.split_at_checked(n).ok_or_else(|| invalid("truncated ZRLE data"))?;
    *s = rest;
    Ok(head)
}

/// CPIXEL: our little-endian 32bpp format with 8-bit channels travels as its low 3 bytes (B, G, R).
fn cpixel(s: &mut &[u8]) -> io::Result<Pixel> {
    let b = take(s, 3)?;
    Ok([b[0], b[1], b[2], 0])
}

fn palette(s: &mut &[u8], n: usize) -> io::Result<Vec<Pixel>> {
    (0..n).map(|_| cpixel(s)).collect()
}

fn lookup(pal: &[Pixel], i: u8) -> io::Result<Pixel> {
    pal.get(i as usize).copied().ok_or_else(|| invalid("ZRLE palette index out of range"))
}

/// Sum of bytes up to and including the first one that is not 255, plus one.
fn run_length(s: &mut &[u8]) -> io::Result<usize> {
    let mut len = 1;
    loop {
        let b = take(s, 1)?[0];
        len += b as usize;
        if b != 255 || len > TILE * TILE {
            return Ok(len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: Pixel = [0, 0, 9, 0];
    const G: Pixel = [0, 9, 0, 0];

    fn pixels(data: &[u8], w: usize, h: usize) -> io::Result<Vec<Pixel>> {
        Ok(decode(data, w, h)?.chunks_exact(4).map(|p| p.try_into().unwrap()).collect())
    }

    #[test]
    fn subencodings() {
        assert_eq!(pixels(&[0, 0, 0, 9, 0, 9, 0], 2, 1).unwrap(), [R, G]);
        assert_eq!(pixels(&[1, 0, 9, 0], 2, 2).unwrap(), [G; 4]);
        // 2-colour palette, 1 bit per pixel, rows padded to a byte: 10, 01.
        assert_eq!(pixels(&[2, 0, 0, 9, 0, 9, 0, 0b1000_0000, 0b0100_0000], 2, 2).unwrap(), [G, R, R, G]);
        // Plain RLE: R×3, G×1.
        assert_eq!(pixels(&[128, 0, 0, 9, 2, 0, 9, 0, 0], 4, 1).unwrap(), [R, R, R, G]);
        // Palette RLE: run of G×2 then a single R.
        assert_eq!(pixels(&[130, 0, 0, 9, 0, 9, 0, 0x81, 1, 0], 3, 1).unwrap(), [G, G, R]);
    }

    #[test]
    fn long_run_and_tile_layout() {
        // 65×1 is a 64-wide tile (R run of 1 + 63) and a 1-wide solid G tile.
        let px = pixels(&[128, 0, 0, 9, 63, 1, 0, 9, 0], 65, 1).unwrap();
        assert_eq!((px[63], px[64]), (R, G));
        // Run of 400 = 1 + 255 + 144 spans a multi-byte length.
        let px = pixels(&[128, 0, 0, 9, 255, 144], 20, 20).unwrap();
        assert!(px.iter().all(|&p| p == R));
    }

    #[test]
    fn rejects_malicious_tiles() {
        assert!(pixels(&[128, 0, 0, 9, 4], 4, 1).is_err(), "run overflows tile");
        assert!(pixels(&[130, 0, 0, 9, 0, 9, 0, 2], 1, 1).is_err(), "index outside palette");
        assert!(pixels(&[17], 1, 1).is_err(), "invalid subencoding");
        assert!(pixels(&[1, 0, 9, 0, 7], 1, 1).is_err(), "trailing bytes");
        assert!(pixels(&[0, 0, 9], 1, 1).is_err(), "truncated");
    }

    #[test]
    fn inflates_across_rectangles() {
        use flate2::{Compress, Compression, FlushCompress};
        let mut c = Compress::new(Compression::default(), true);
        let mut z = Decompress::new(true);
        for rect in [&[1u8, 0, 9, 0][..], &[1, 0, 0, 9]] {
            let mut wire = Vec::with_capacity(64);
            c.compress_vec(rect, &mut wire, FlushCompress::Sync).unwrap();
            assert_eq!(inflate(&mut z, &wire, max_inflated(1, 1)).unwrap(), rect);
        }
    }
}
