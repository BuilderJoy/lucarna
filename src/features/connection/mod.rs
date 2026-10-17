//! Connection: reaching a server and authenticating.

mod address;
mod auth;
pub mod handshake;
mod view;

pub use address::ServerAddress;
pub use view::ConnectForm;
