//! UltraVNC file transfer: browse the server's drives and download files (protocol: docs/ultravnc-ft.md).

mod protocol;
mod transfer;
mod view;

pub use protocol::{read, ENC_PROTOCOL_VERSION};
pub use transfer::{Receiver, RemoteFiles, Update};
pub use view::FilesView;
