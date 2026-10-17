use std::io;

use crate::shared::wire::invalid;

const MAX_DIM: usize = 16384;
const MAX_CURSOR: usize = 256;

/// The server's mouse cursor shape, sent as a pseudo-rectangle so we can draw it locally without lag.
#[derive(Debug, PartialEq)]
pub struct Cursor {
    /// Remote pixel inside the image that sits under the pointer.
    pub hotspot: (usize, usize),
    pub size: [usize; 2],
    /// RGBA; empty when the server hides the cursor.
    pub rgba: Vec<u8>,
}

impl Cursor {
    /// `pixels` are BGRX (our pixel format) and `mask` has one bit per pixel, rows padded to a byte, MSB first.
    pub fn new(hotspot: (usize, usize), [w, h]: [usize; 2], pixels: &[u8], mask: &[u8]) -> Self {
        let row = w.div_ceil(8);
        let rgba = (0..w * h)
            .flat_map(|i| {
                let p = &pixels[i * 4..i * 4 + 4];
                let opaque = mask[(i / w) * row + (i % w) / 8] >> (7 - i % w % 8) & 1;
                [p[2], p[1], p[0], opaque * 255]
            })
            .collect();
        Self { hotspot, size: [w, h], rgba }
    }

    /// Wire byte counts for a w×h cursor: (pixel bytes, mask bytes). Rejects sizes above `MAX_CURSOR`.
    pub fn wire_sizes(w: usize, h: usize) -> io::Result<(usize, usize)> {
        if w > MAX_CURSOR || h > MAX_CURSOR {
            return Err(invalid(format!("cursor too large {w}x{h}")));
        }
        Ok((w * h * 4, w.div_ceil(8) * h))
    }
}

/// The remote desktop image. Pixels are BGRX (32bpp little-endian, red shift 16).
pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    /// Region changed since the last upload, as [x0, y0, x1, y1] (exclusive).
    pub dirty: Option<[usize; 4]>,
    /// A new cursor shape the view hasn't picked up yet.
    pub cursor: Option<Cursor>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> io::Result<Self> {
        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
            return Err(invalid(format!("unsupported screen size {width}x{height}")));
        }
        Ok(Self { width, height, pixels: vec![0; width * height * 4], dirty: Some([0, 0, width, height]), cursor: None })
    }

    pub fn size(&self) -> (u16, u16) {
        (self.width as u16, self.height as u16)
    }

    pub fn contains(&self, x: usize, y: usize, w: usize, h: usize) -> bool {
        x + w <= self.width && y + h <= self.height
    }

    pub fn put(&mut self, x: usize, y: usize, w: usize, h: usize, data: &[u8]) {
        let row = w * 4;
        for r in 0..h {
            let dst = ((y + r) * self.width + x) * 4;
            self.pixels[dst..dst + row].copy_from_slice(&data[r * row..(r + 1) * row]);
        }
        let [x0, y0, x1, y1] = self.dirty.unwrap_or([x, y, x + w, y + h]);
        self.dirty = Some([x0.min(x), y0.min(y), x1.max(x + w), y1.max(y + h)]);
    }

    pub fn get(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let row = w * 4;
        let mut out = Vec::with_capacity(row * h);
        for r in 0..h {
            let src = ((y + r) * self.width + x) * 4;
            out.extend_from_slice(&self.pixels[src..src + row]);
        }
        out
    }

    /// The region as RGBA, ready for a texture.
    pub fn rgba(&self, [x0, y0, x1, y1]: [usize; 4]) -> Vec<u8> {
        self.get(x0, y0, x1 - x0, y1 - y0).chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_rect_overlap() {
        let mut fb = Framebuffer::new(3, 1).unwrap();
        fb.put(0, 0, 3, 1, &[1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]);
        let data = fb.get(0, 0, 2, 1);
        fb.put(1, 0, 2, 1, &data);
        assert_eq!(fb.pixels, [1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2]);
    }

    #[test]
    fn dirty_region_grows() {
        let mut fb = Framebuffer::new(10, 10).unwrap();
        fb.dirty = None;
        fb.put(2, 3, 1, 1, &[0; 4]);
        fb.put(5, 1, 2, 2, &[0; 16]);
        assert_eq!(fb.dirty, Some([2, 1, 7, 4]));
    }

    #[test]
    fn cursor_mask_selects_opaque_pixels() {
        // 9×1 cursor: the mask row spans two bytes. Pixels 0 and 8 are opaque, the rest transparent.
        let pixels: Vec<u8> = (0..9).flat_map(|i| [i, 20, 30, 0]).collect();
        let c = Cursor::new((1, 0), [9, 1], &pixels, &[0b1000_0000, 0b1000_0000]);
        let alpha: Vec<u8> = c.rgba.chunks(4).map(|p| p[3]).collect();
        assert_eq!(alpha, [255, 0, 0, 0, 0, 0, 0, 0, 255]);
        assert_eq!(&c.rgba[..4], [30, 20, 0, 255], "BGRX becomes RGBA");
        assert_eq!(Cursor::wire_sizes(9, 3).unwrap(), (108, 6));
        assert!(Cursor::wire_sizes(257, 1).is_err());
    }

    #[test]
    fn rejects_huge_screen() {
        assert!(Framebuffer::new(65535, 65535).is_err());
    }
}
