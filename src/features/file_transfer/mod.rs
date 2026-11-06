//! UltraVNC file transfer: browse the server's drives and download files (protocol: docs/ultravnc-ft.md).

mod protocol;
mod transfer;
mod view;

pub use protocol::{ENC_PROTOCOL_VERSION, Entry, read};
pub use transfer::{DualFiles, FileOps, Receiver, RemoteFiles, Update, local_name};
pub use view::FilesView;
