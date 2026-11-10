//! Linux screen capture straight from the X server, instead of an `import` process per frame.

use std::io;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat, ImageOrder};
use x11rb::rust_connection::RustConnection;

use super::capture::Frame;

/// An open connection to the X server and its root window.
pub struct X11Screen {
    conn: RustConnection,
    root: u32,
}

impl X11Screen {
    pub fn open() -> io::Result<Self> {
        let (conn, number) = x11rb::connect(None).map_err(io::Error::other)?;
        let setup = conn.setup();
        let screen = setup
            .roots
            .get(number)
            .ok_or_else(|| io::Error::other("X server has no screen"))?;
        // Only the usual layout: 24-bit colour in 32-bit little-endian pixels, which is already the host's BGRX.
        // Anything else keeps using `import`.
        let bgrx = setup.image_byte_order == ImageOrder::LSB_FIRST
            && screen.root_depth == 24
            && setup
                .pixmap_formats
                .iter()
                .any(|f| f.depth == 24 && f.bits_per_pixel == 32);
        if !bgrx {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "X server pixel layout is not 32-bit BGRX",
            ));
        }
        let root = screen.root;
        Ok(Self { conn, root })
    }

    // ponytail: GetImage copies the picture through the socket (a few ms at 1080p). MIT-SHM removes that copy;
    // add it if the Share page shows capture above 8 ms on a real desktop.
    pub fn grab(&self) -> io::Result<Frame> {
        let other = io::Error::other::<x11rb::errors::ReplyError>;
        let size = self
            .conn
            .get_geometry(self.root)
            .map_err(io::Error::other)?
            .reply()
            .map_err(other)?;
        let image = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                0,
                0,
                size.width,
                size.height,
                !0,
            )
            .map_err(io::Error::other)?
            .reply()
            .map_err(other)?;
        Frame::new(size.width, size.height, image.data)
    }
}
