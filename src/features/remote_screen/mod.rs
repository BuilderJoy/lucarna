//! Remote screen: receiving and showing the server's framebuffer.

mod framebuffer;
mod hextile;
mod protocol;
mod view;
mod zrle;

pub use framebuffer::Framebuffer;
pub use protocol::{negotiate, Decoder, read_update, request_update, skip_colour_map};
pub use view::{ScreenView, Viewport};
