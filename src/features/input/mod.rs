//! Input: forwarding local keyboard and mouse to the server.

mod keysym;
mod protocol;
mod view;

pub use protocol::{ALTGR_SYMBOLS, SHORTCUTS, combo, ctrl_alt_del, type_char};
pub use view::InputForwarder;
