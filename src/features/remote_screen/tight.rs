//! Tight encoding (7), following libvncclient's `tight.c`: a fill colour, a JPEG, or pixels behind an
//! optional copy / palette / gradient filter, zlib-compressed in one of four session-long streams.
//! With our 32bpp depth-24 format every colour (TPIXEL) travels as 3 bytes in R, G, B order.

use std::io::{self, Read};

use flate2::Decompress;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};

use super::zrle;
use crate::shared::wire::{invalid, read_bytes, read_u8};

const FILL: u8 = 8;
const JPEG: u8 = 9;
const EXPLICIT_FILTER: u8 = 4;
const COPY: u8 = 0;
const PALETTE: u8 = 1;
const GRADIENT: u8 = 2;
/// Below this many bytes the pixel data is sent as is, without zlib or a length.
const MIN_TO_COMPRESS: usize = 12;

pub struct Tight {
    streams: [Decompress; 4],
}

impl Default for Tight {
    fn default() -> Self {
        Self { streams: [0; 4].map(|_| Decompress::new(true)) }
    }
}

impl Tight {
    /// Reads one w×h rectangle as BGRX pixels.
    pub fn read(&mut self, s: &mut impl Read, w: usize, h: usize) -> io::Result<Vec<u8>> {
        let control = read_u8(s)?;
        // The low four bits ask us to reset the matching zlib streams.
        for (i, z) in self.streams.iter_mut().enumerate() {
            if control >> i & 1 == 1 {
                z.reset(true);
            }
        }
        let kind = control >> 4;
        match kind {
            FILL => Ok(bgrx(&read_bytes(s, 3)?).repeat(w * h)),
            JPEG => {
                let len = compact_len(s)?;
                jpeg(&read_bytes(s, len)?, w, h)
            }
            0..=7 => self.basic(s, kind, w, h),
            _ => Err(invalid(format!("invalid Tight control byte {control:#04x}"))),
        }
    }

    fn basic(&mut self, s: &mut impl Read, kind: u8, w: usize, h: usize) -> io::Result<Vec<u8>> {
        let filter = if kind & EXPLICIT_FILTER != 0 { read_u8(s)? } else { COPY };
        let palette: Vec<[u8; 4]> = if filter == PALETTE {
            let n = read_u8(s)? as usize + 1;
            if n < 2 {
                return Err(invalid("Tight palette with fewer than 2 colours"));
            }
            read_bytes(s, n * 3)?.chunks(3).map(bgrx).collect()
        } else {
            Vec::new()
        };
        let row = match filter {
            PALETTE if palette.len() == 2 => w.div_ceil(8),
            PALETTE => w,
            COPY | GRADIENT => w * 3,
            f => return Err(invalid(format!("unknown Tight filter {f}"))),
        };
        let size = row * h;
        let data = if size < MIN_TO_COMPRESS {
            read_bytes(s, size)?
        } else {
            let len = compact_len(s)?;
            let data = zrle::inflate(&mut self.streams[usize::from(kind & 3)], &read_bytes(s, len)?, size)?;
            if data.len() != size {
                return Err(invalid("Tight data has the wrong size"));
            }
            data
        };
        match filter {
            PALETTE if palette.len() == 2 => Ok((0..w * h).flat_map(|i| palette[usize::from(data[(i / w) * row + (i % w) / 8] >> (7 - i % w % 8) & 1)]).collect()),
            PALETTE => data
                .iter()
                .map(|&i| palette.get(usize::from(i)).copied().ok_or_else(|| invalid("Tight palette index out of range")))
                .collect::<io::Result<Vec<_>>>()
                .map(|px| px.concat()),
            GRADIENT => Ok(gradient(&data, w).chunks(3).flat_map(bgrx).collect()),
            _ => Ok(data.chunks(3).flat_map(bgrx).collect()),
        }
    }
}

/// TPIXEL (R, G, B) to our BGRX.
fn bgrx(p: &[u8]) -> [u8; 4] {
    [p[2], p[1], p[0], 0]
}

/// 1 to 3 bytes, 7 bits each except the last, which gives 8: up to 4 MiB - 1.
fn compact_len(s: &mut impl Read) -> io::Result<usize> {
    let mut len = 0;
    for shift in [0, 7] {
        let b = usize::from(read_u8(s)?);
        len |= (b & 0x7F) << shift;
        if b & 0x80 == 0 {
            return Ok(len);
        }
    }
    Ok(len | usize::from(read_u8(s)?) << 14)
}

/// Each component was sent as the difference from `above + left - above_left` (clamped), mod 256.
fn gradient(data: &[u8], w: usize) -> Vec<u8> {
    let stride = w * 3;
    let mut rgb = vec![0u8; data.len()];
    for i in 0..data.len() {
        let (first_column, first_row) = (i % stride < 3, i < stride);
        let above = if first_row { 0 } else { i32::from(rgb[i - stride]) };
        let left = if first_column { 0 } else { i32::from(rgb[i - 3]) };
        let above_left = if first_row || first_column { 0 } else { i32::from(rgb[i - stride - 3]) };
        rgb[i] = ((above + left - above_left).clamp(0, 255) as u8).wrapping_add(data[i]);
    }
    rgb
}

fn jpeg(data: &[u8], w: usize, h: usize) -> io::Result<Vec<u8>> {
    // The size limits make the decoder refuse a bigger image before allocating for it.
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB).set_max_width(w).set_max_height(h);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    let rgb = decoder.decode().map_err(|e| invalid(format!("Tight JPEG: {e:?}")))?;
    let info = decoder.info().ok_or_else(|| invalid("Tight JPEG without headers"))?;
    if (usize::from(info.width), usize::from(info.height)) != (w, h) || rgb.len() != w * h * 3 {
        return Err(invalid("Tight JPEG has the wrong size"));
    }
    Ok(rgb.chunks(3).flat_map(bgrx).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    const R: [u8; 4] = [0, 0, 9, 0];
    const G: [u8; 4] = [0, 9, 0, 0];

    fn decode(t: &mut Tight, msg: &[u8], w: usize, h: usize) -> io::Result<Vec<[u8; 4]>> {
        let mut s = msg;
        let px = t.read(&mut s, w, h)?.chunks(4).map(|p| p.try_into().unwrap()).collect();
        assert!(s.is_empty(), "unread bytes");
        Ok(px)
    }

    #[test]
    fn compact_lengths() {
        assert_eq!(compact_len(&mut &[0x05][..]).unwrap(), 5);
        assert_eq!(compact_len(&mut &[0x80 | 0x10, 0x01][..]).unwrap(), 0x10 | 1 << 7);
        assert_eq!(compact_len(&mut &[0xFF, 0xFF, 0xFF][..]).unwrap(), (1 << 22) - 1);
    }

    #[test]
    fn fill_and_small_uncompressed_copy() {
        let mut t = Tight::default();
        assert_eq!(decode(&mut t, &[FILL << 4, 9, 0, 0], 2, 2).unwrap(), [R; 4]);
        // 2×1 copy is 6 bytes < 12: raw RGB, no length, no zlib.
        assert_eq!(decode(&mut t, &[0x00, 9, 0, 0, 0, 9, 0], 2, 1).unwrap(), [R, G]);
    }

    #[test]
    fn palettes() {
        let mut t = Tight::default();
        // Two colours, 1 bit per pixel, rows padded to a byte: 10, 01.
        let msg = [(EXPLICIT_FILTER) << 4, PALETTE, 1, 9, 0, 0, 0, 9, 0, 0b1000_0000, 0b0100_0000];
        assert_eq!(decode(&mut t, &msg, 2, 2).unwrap(), [G, R, R, G]);
        // Three colours, one byte per pixel; index 3 does not exist.
        let three = [EXPLICIT_FILTER << 4, PALETTE, 2, 9, 0, 0, 0, 9, 0, 0, 0, 9];
        assert_eq!(decode(&mut t, &[&three[..], &[1, 0]].concat(), 2, 1).unwrap(), [G, R]);
        assert!(decode(&mut t, &[&three[..], &[3, 0]].concat(), 2, 1).is_err());
    }

    #[test]
    fn gradient_predicts_from_neighbours() {
        // 2×2 of one component pattern: decoded values 10, 20 / 30, 45 for every channel.
        // Sent differences: 10; 20-10=10; 30-10=20; 45-(30+20-10)=5.
        let want = [10u8, 20, 30, 45];
        let diffs = [10u8, 10, 20, 5];
        let data: Vec<u8> = diffs.iter().flat_map(|&d| [d, d, d]).collect();
        let rgb = gradient(&data, 2);
        assert_eq!(rgb.chunks(3).map(|p| p[0]).collect::<Vec<_>>(), want);
    }

    #[test]
    fn zlib_streams_persist_and_reset() {
        let mut t = Tight::default();
        let mut z = Compress::new(Compression::default(), true);
        let rect = |z: &mut Compress, control: u8| {
            let raw = [9u8, 0, 0].repeat(4); // 4×1 red: 12 bytes, so it goes through zlib
            let mut packed = Vec::with_capacity(64);
            z.compress_vec(&raw, &mut packed, FlushCompress::Sync).unwrap();
            [&[control, packed.len() as u8][..], &packed].concat()
        };
        // Stream 1 (control 0x10) twice on the same zlib stream, then a reset of stream 1 (bit 1).
        assert_eq!(decode(&mut t, &rect(&mut z, 0x10), 4, 1).unwrap(), [R; 4]);
        assert_eq!(decode(&mut t, &rect(&mut z, 0x10), 4, 1).unwrap(), [R; 4]);
        let mut fresh = Compress::new(Compression::default(), true);
        assert_eq!(decode(&mut t, &rect(&mut fresh, 0x12), 4, 1).unwrap(), [R; 4]);
    }

    #[test]
    fn rejects_bad_input() {
        let mut t = Tight::default();
        assert!(decode(&mut t, &[0xA0], 1, 1).is_err(), "control 10 is not supported");
        assert!(decode(&mut t, &[EXPLICIT_FILTER << 4, 7], 1, 1).is_err(), "unknown filter");
        assert!(decode(&mut t, &[EXPLICIT_FILTER << 4, PALETTE, 0, 1, 2, 3], 1, 1).is_err(), "one-colour palette");
    }

    /// 16×8 JPEG, red left half and blue right half, made with ImageMagick at quality 100 without chroma subsampling.
    #[test]
    fn jpeg_decodes_to_bgrx() {
        let file = include_bytes!("../../../tests/fixtures/tight-red-blue-16x8.jpg");
        let mut msg = vec![JPEG << 4, 0x80 | (file.len() & 0x7F) as u8, (file.len() >> 7) as u8];
        msg.extend_from_slice(file);
        let px = decode(&mut Tight::default(), &msg, 16, 8).unwrap();
        let close = |p: [u8; 4], q: [u8; 3]| p[..3].iter().zip(q).all(|(a, b)| a.abs_diff(b) < 8);
        assert!(close(px[0], [0, 0, 255]), "red as BGRX, got {:?}", px[0]);
        assert!(close(px[15], [255, 0, 0]), "blue as BGRX, got {:?}", px[15]);
        // A JPEG of another size is refused.
        assert!(decode(&mut Tight::default(), &msg, 8, 8).is_err());
    }
}
