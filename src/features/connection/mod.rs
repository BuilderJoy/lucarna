//! Connection: reaching a server and authenticating.

mod address;
mod auth;
pub mod handshake;
mod mslogon;
mod saved;
mod view;

pub use address::ServerAddress;
pub use auth::Credentials;
pub use saved::SessionOptions;
pub use view::{ConnectRequest, Dashboard};
