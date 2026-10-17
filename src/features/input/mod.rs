//! Input: forwarding local keyboard and mouse to the server.

mod keysym;
mod protocol;
mod view;

pub use protocol::{combo, ctrl_alt_del, SHORTCUTS};
pub use view::InputForwarder;
