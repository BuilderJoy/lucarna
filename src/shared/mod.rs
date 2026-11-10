//! Shared kernel: what every feature uses and no feature owns.
//!
//! | File | What it holds |
//! |---|---|
//! | `wire.rs` | Reading big-endian numbers and bytes from a socket, and the `invalid` error for bad data. |
//! | `theme.rs` | Light and dark colours and the shared buttons and controls. |
//! | `i18n.rs` | English and Spanish text: `tr("...")` and the `ES` table. |
//! | `crypto.rs` | End-to-end encryption with a PIN. |

pub mod crypto;
pub mod i18n;
pub mod theme;
pub mod wire;
