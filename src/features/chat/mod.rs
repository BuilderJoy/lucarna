//! Text chat with the remote user (UltraVNC TextChat).

mod protocol;
mod view;

pub use protocol::{Msg, read};
pub use view::ChatView;
