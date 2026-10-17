use std::io;

use crate::shared::wire::invalid;

const MAX_DIM: usize = 16384;

/// The remote desktop image. Pixels are BGRX (32bpp little-endian, red shift 16).
pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    /// Region changed since the last upload, as [x0, y0, x1, y1] (exclusive).
    pub dirty: Option<[usize; 4]>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> io::Result<Self> {
        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
            return Err(invalid(format!("unsupported screen size {width}x{height}")));
        }
        Ok(Self { width, height, pixels: vec![0; width * height * 4], dirty: Some([0, 0, width, height]) })
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
    fn rejects_huge_screen() {
        assert!(Framebuffer::new(65535, 65535).is_err());
    }
}
