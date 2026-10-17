//! Input: forwarding local keyboard and mouse to the server.

mod keysym;
mod protocol;
mod view;

pub use protocol::ctrl_alt_del;
pub use view::InputForwarder;
