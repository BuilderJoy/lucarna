//! Vertical slices: each feature owns its domain types, its wire messages (`protocol.rs`) and its UI
//! (`view.rs`). Other code may only use what a feature's `mod.rs` re-exports.
//!
//! | Folder | What it does |
//! |---|---|
//! | `connection` | The start screen (Address Book), saved connections, and the RFB handshake with its password schemes. |
//! | `remote_screen` | Receives the server's picture: the `Framebuffer`, one decoder per encoding (Tight, ZRLE, Hextile...), and the widget that paints it. |
//! | `input` | Sends the local keyboard and mouse to the server. |
//! | `clipboard` | Copies text between the two computers. |
//! | `file_transfer` | The Files window and UltraVNC file transfer (RFB message 7). |
//! | `sftp` | The same Files window over SFTP when the connection uses SSH. |
//! | `ssh` | The SSH tunnel, run with the system's `ssh` command. |
//! | `chat` | Text chat with the person at the server. |
//! | `host` | The other direction: sharing this computer. Start at its `mod.rs`. |

pub mod chat;
pub mod clipboard;
pub mod connection;
pub mod file_transfer;
pub mod host;
pub mod input;
pub mod remote_screen;
pub mod sftp;
pub mod ssh;
