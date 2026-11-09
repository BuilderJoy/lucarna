//! Screen capture and 64×64 dirty-tile damage tracking in user space.

use std::io;
use std::process::Command;

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};

use crate::shared::wire::invalid;

pub const TILE_SIZE: u16 = 64;
const MAX_DIM: u16 = 16384;

/// A captured desktop frame in Lucarna's native 32bpp BGRX format (B, G, R, 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u8>,
}

impl Frame {
    pub fn new(width: u16, height: u16, pixels: Vec<u8>) -> io::Result<Self> {
        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
            return Err(invalid(format!(
                "invalid frame dimensions {width}x{height}"
            )));
        }
        let expected = usize::from(width) * usize::from(height) * 4;
        if pixels.len() != expected {
            return Err(invalid(format!(
                "frame buffer length {} does not match {width}x{height}x4 ({expected})",
                pixels.len()
            )));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Creates a solid-colour BGRX frame.
    pub fn solid(width: u16, height: u16, bgrx: [u8; 4]) -> Self {
        let count = usize::from(width) * usize::from(height);
        let mut pixels = Vec::with_capacity(count * 4);
        for _ in 0..count {
            pixels.extend_from_slice(&bgrx);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Writes a single BGRX pixel at `(x, y)`.
    pub fn set_pixel(&mut self, x: u16, y: u16, bgrx: [u8; 4]) {
        if x < self.width && y < self.height {
            let idx = (usize::from(y) * usize::from(self.width) + usize::from(x)) * 4;
            self.pixels[idx..idx + 4].copy_from_slice(&bgrx);
        }
    }

    /// Extracts the BGRX bytes of `rect` tightly packed row by row.
    pub fn region(&self, rect: DirtyRect) -> Vec<u8> {
        let row_bytes = usize::from(rect.w) * 4;
        let stride = usize::from(self.width) * 4;
        let mut out = Vec::with_capacity(row_bytes * usize::from(rect.h));
        for row in 0..usize::from(rect.h) {
            let y = usize::from(rect.y) + row;
            let start = y * stride + usize::from(rect.x) * 4;
            out.extend_from_slice(&self.pixels[start..start + row_bytes]);
        }
        out
    }
}

/// A modified screen rectangle in pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// Compares consecutive frames on a 64×64 grid and coalesces horizontally adjacent changed tiles.
/// Keeping unmodified tiles out of the encoder prevents CPU spikes that could disturb live audio.
#[derive(Default)]
pub struct DamageTracker {
    prev: Option<Frame>,
    force_full: bool,
}

impl DamageTracker {
    pub fn new() -> Self {
        Self {
            prev: None,
            force_full: true,
        }
    }

    /// Forces the next `diff` call to return the entire screen (e.g. non-incremental RFB request).
    pub fn mark_full_dirty(&mut self) {
        self.force_full = true;
    }

    /// Returns the dirty rectangles between the previous frame and `next`, updating the reference frame.
    pub fn diff(&mut self, next: &Frame) -> Vec<DirtyRect> {
        let Some(prev) = &mut self.prev else {
            self.prev = Some(next.clone());
            self.force_full = false;
            return vec![DirtyRect {
                x: 0,
                y: 0,
                w: next.width,
                h: next.height,
            }];
        };

        if self.force_full || prev.width != next.width || prev.height != next.height {
            *prev = next.clone();
            self.force_full = false;
            return vec![DirtyRect {
                x: 0,
                y: 0,
                w: next.width,
                h: next.height,
            }];
        }

        let mut rects = Vec::new();
        let stride = usize::from(next.width) * 4;

        let mut ty = 0u16;
        while ty < next.height {
            let th = TILE_SIZE.min(next.height - ty);
            let mut run_start: Option<u16> = None;
            let mut run_w = 0u16;

            let mut tx = 0u16;
            while tx < next.width {
                let tw = TILE_SIZE.min(next.width - tx);
                if tile_changed(&prev.pixels, &next.pixels, stride, tx, ty, tw, th) {
                    copy_tile(&mut prev.pixels, &next.pixels, stride, tx, ty, tw, th);
                    match run_start {
                        Some(_) => run_w += tw,
                        None => {
                            run_start = Some(tx);
                            run_w = tw;
                        }
                    }
                } else if let Some(sx) = run_start.take() {
                    rects.push(DirtyRect {
                        x: sx,
                        y: ty,
                        w: run_w,
                        h: th,
                    });
                }
                tx += TILE_SIZE;
            }

            if let Some(sx) = run_start.take() {
                rects.push(DirtyRect {
                    x: sx,
                    y: ty,
                    w: run_w,
                    h: th,
                });
            }
            ty += TILE_SIZE;
        }

        rects
    }
}

fn tile_changed(
    prev: &[u8],
    next: &[u8],
    stride: usize,
    tx: u16,
    ty: u16,
    tw: u16,
    th: u16,
) -> bool {
    let row_bytes = usize::from(tw) * 4;
    let x_off = usize::from(tx) * 4;
    for r in 0..usize::from(th) {
        let start = (usize::from(ty) + r) * stride + x_off;
        if prev[start..start + row_bytes] != next[start..start + row_bytes] {
            return true;
        }
    }
    false
}

fn copy_tile(prev: &mut [u8], next: &[u8], stride: usize, tx: u16, ty: u16, tw: u16, th: u16) {
    let row_bytes = usize::from(tw) * 4;
    let x_off = usize::from(tx) * 4;
    for r in 0..usize::from(th) {
        let start = (usize::from(ty) + r) * stride + x_off;
        prev[start..start + row_bytes].copy_from_slice(&next[start..start + row_bytes]);
    }
}

/// Source of desktop frames in user space.
pub trait ScreenCapturer: Send {
    fn capture(&mut self) -> io::Result<Frame>;
}

/// Deterministic in-memory screen capturer for tests and headless verification.
pub struct SyntheticCapturer {
    frames: Vec<Frame>,
    cursor: usize,
}

impl SyntheticCapturer {
    pub fn new(frames: Vec<Frame>) -> Self {
        assert!(
            !frames.is_empty(),
            "SyntheticCapturer requires at least one frame"
        );
        Self { frames, cursor: 0 }
    }
}

impl ScreenCapturer for SyntheticCapturer {
    fn capture(&mut self) -> io::Result<Frame> {
        let idx = self.cursor.min(self.frames.len() - 1);
        if self.cursor + 1 < self.frames.len() {
            self.cursor += 1;
        }
        Ok(self.frames[idx].clone())
    }
}

/// User-space OS screen capturer (no root/SYSTEM service required).
/// Uses native user-session tools to grab a snapshot, falling back to an animated Studio VU-meter pattern
/// if screen-recording permission has not been granted yet.
pub struct SystemCapturer {
    fallback_size: (u16, u16),
    tick: u16,
}

impl Default for SystemCapturer {
    fn default() -> Self {
        Self::new((1280, 720))
    }
}

impl SystemCapturer {
    pub fn new(fallback_size: (u16, u16)) -> Self {
        Self {
            fallback_size,
            tick: 0,
        }
    }

    fn studio_test_frame(&mut self) -> Frame {
        let (w, h) = self.fallback_size;
        let mut frame = Frame::solid(w, h, [28, 22, 20, 0]);
        self.tick = self.tick.wrapping_add(8);
        let bar_w = (self.tick % (w.saturating_sub(128).max(64))) + 32;
        for y in 64..128.min(h) {
            for x in 64..(64 + bar_w).min(w) {
                // Green-to-amber VU meter bar in BGRX.
                let color = if x < w / 2 {
                    [80, 210, 60, 0]
                } else {
                    [40, 170, 245, 0]
                };
                frame.set_pixel(x, y, color);
            }
        }
        frame
    }
}

impl ScreenCapturer for SystemCapturer {
    fn capture(&mut self) -> io::Result<Frame> {
        capture_os_screen().or_else(|_| Ok(self.studio_test_frame()))
    }
}

/// Parses a binary P6 PPM image (`P6 <width> <height> 255\n<RGB bytes>`) into a BGRX `Frame`.
pub fn parse_ppm_p6(data: &[u8]) -> io::Result<Frame> {
    if !data.starts_with(b"P6") {
        return Err(invalid("not a binary P6 PPM image"));
    }
    let mut pos = 2;
    let mut tokens = [0usize; 3];
    for tok in &mut tokens {
        while pos < data.len() && (data[pos].is_ascii_whitespace() || data[pos] == b'#') {
            if data[pos] == b'#' {
                while pos < data.len() && data[pos] != b'\n' {
                    pos += 1;
                }
            } else {
                pos += 1;
            }
        }
        let start = pos;
        while pos < data.len() && data[pos].is_ascii_digit() {
            pos += 1;
        }
        if start == pos {
            return Err(invalid("truncated PPM header"));
        }
        *tok = std::str::from_utf8(&data[start..pos])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| invalid("invalid PPM header number"))?;
    }
    if pos >= data.len() || !data[pos].is_ascii_whitespace() {
        return Err(invalid("missing whitespace after PPM maxval"));
    }
    pos += 1; // single whitespace byte after maxval (255)
    let [w, h, maxval] = tokens;
    if maxval != 255 || w == 0 || h == 0 || w > usize::from(MAX_DIM) || h > usize::from(MAX_DIM) {
        return Err(invalid("unsupported PPM dimensions or maxval"));
    }
    let rgb_len = w * h * 3;
    if data.len() < pos + rgb_len {
        return Err(invalid("truncated PPM pixel data"));
    }
    let mut pixels = Vec::with_capacity(w * h * 4);
    for rgb in data[pos..pos + rgb_len].chunks_exact(3) {
        pixels.extend_from_slice(&[rgb[2], rgb[1], rgb[0], 0]);
    }
    Frame::new(w as u16, h as u16, pixels)
}

fn capture_os_screen() -> io::Result<Frame> {
    super::os_input::capture()
}

/// Linux: ImageMagick `import` writes the root window as a P6 PPM on stdout.
pub(super) fn capture_with_import() -> io::Result<Frame> {
    let out = Command::new("import")
        .args(["-window", "root", "ppm:-"])
        .output()?;
    if out.status.success() {
        return parse_ppm_p6(&out.stdout);
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "OS capture tool unavailable",
    ))
}

/// macOS: `screencapture` needs the Screen Recording permission, or it returns the wallpaper only.
pub(super) fn capture_with_screencapture() -> io::Result<Frame> {
    let tmp = std::env::temp_dir().join(format!("lucarna-cap-{}.jpg", std::process::id()));
    let status = Command::new("screencapture")
        .args(["-x", "-m", "-t", "jpg"])
        .arg(&tmp)
        .status()?;
    if status.success() {
        let bytes = std::fs::read(&tmp)?;
        let _ = std::fs::remove_file(&tmp);
        return decode_jpeg_frame(&bytes);
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "OS capture tool unavailable",
    ))
}

fn decode_jpeg_frame(data: &[u8]) -> io::Result<Frame> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(usize::from(MAX_DIM))
        .set_max_height(usize::from(MAX_DIM));
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    let rgb = decoder
        .decode()
        .map_err(|e| invalid(format!("JPEG capture decode error: {e:?}")))?;
    let info = decoder
        .info()
        .ok_or_else(|| invalid("JPEG capture missing header info"))?;
    let (w, h) = (usize::from(info.width), usize::from(info.height));
    // Downscale Retina 2x captures above 1600px wide by 2× for snappy interactive framing.
    let step = if w > 1600 { 2 } else { 1 };
    let (out_w, out_h) = (w / step, h / step);
    let mut pixels = Vec::with_capacity(out_w * out_h * 4);
    for y in 0..out_h {
        let row = (y * step) * w * 3;
        for x in 0..out_w {
            let i = row + (x * step) * 3;
            pixels.extend_from_slice(&[rgb[i + 2], rgb[i + 1], rgb[i], 0]);
        }
    }
    Frame::new(out_w as u16, out_h as u16, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_tracker_emits_only_modified_64x64_tiles_and_coalesces_rows() {
        let mut tracker = DamageTracker::new();
        let f1 = Frame::solid(192, 128, [10, 20, 30, 0]);
        // First frame is always a single full-screen rectangle.
        assert_eq!(
            tracker.diff(&f1),
            vec![DirtyRect {
                x: 0,
                y: 0,
                w: 192,
                h: 128
            }]
        );
        // Identical second frame yields zero dirty rectangles (0 CPU/network overhead).
        assert!(tracker.diff(&f1).is_empty());

        // Modify two adjacent tiles on the top row (tile (0,0) and tile (1,0)) and one on the bottom row (tile (2,1)).
        let mut f2 = f1.clone();
        f2.set_pixel(5, 5, [255, 0, 0, 0]);
        f2.set_pixel(70, 5, [255, 0, 0, 0]);
        f2.set_pixel(140, 80, [0, 255, 0, 0]);

        let rects = tracker.diff(&f2);
        assert_eq!(
            rects,
            vec![
                // Tiles (0,0) and (1,0) coalesced into a 128×64 rectangle.
                DirtyRect {
                    x: 0,
                    y: 0,
                    w: 128,
                    h: 64
                },
                // Tile (2,1) isolated on row 1.
                DirtyRect {
                    x: 128,
                    y: 64,
                    w: 64,
                    h: 64
                },
            ]
        );
    }

    #[test]
    fn ppm_p6_parser_converts_rgb_to_bgrx() {
        let mut ppm = b"P6\n# comment\n2 1\n255\n".to_vec();
        ppm.extend_from_slice(&[10, 20, 30, 40, 50, 60]);
        let frame = parse_ppm_p6(&ppm).unwrap();
        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.pixels, vec![30, 20, 10, 0, 60, 50, 40, 0]);
    }
}
