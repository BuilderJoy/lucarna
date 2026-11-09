//! UltraVNC file transfer: browse the server's drives and download files (protocol: docs/ultravnc-ft.md).

mod protocol;
mod transfer;
mod view;

#[cfg(test)]
pub use protocol::Msg;
pub use protocol::{
    A_DIR_CREATE, A_DIR_INACCESSIBLE, A_DIRECTORY, A_FILE, A_FILE_DELETE, A_FILE_RENAME,
    ENC_PROTOCOL_VERSION, Entry, MAX_BLOCK, UNICODE, read,
};
pub use transfer::{DualFiles, FileOps, Receiver, RemoteFiles, Update, local_name};
pub use view::FilesView;
