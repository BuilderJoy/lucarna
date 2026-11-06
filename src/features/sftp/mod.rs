//! SFTP v3 client over native SSH subsystem (Fase 8.2 del Roadmap).
//! Provides secure file transfer even when VNC server disables file transfer policies.

pub mod client;
pub mod protocol;

pub use client::SftpClient;
