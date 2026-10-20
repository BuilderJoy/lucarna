//! Connection: reaching a server and authenticating.

mod address;
mod auth;
pub mod handshake;
mod mslogon;
mod saved;
mod view;

pub use address::{ServerAddress, SshTarget};
pub use auth::Credentials;
pub use saved::{SessionOptions, ssh_password};
pub use view::{ConnectRequest, Dashboard};
