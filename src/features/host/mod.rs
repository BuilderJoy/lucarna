//! Embedded RFB Host (`src/features/host/`): user-space screen capture, 64×64 dirty-tile
//! ZRLE encoding, input injection with local mouse priority, native RFB message type 7
//! file transfer server (`ft_server`), and RFB 3.8 server session loop.

#![allow(dead_code, unused_imports)]

pub mod capture;
pub mod ft_server;
pub mod input_inject;
#[cfg(target_os = "macos")]
mod mac_capture;
pub mod os_input;
pub mod session;
pub mod view;

pub use capture::{
    DamageTracker, DirtyRect, Frame, ScreenCapturer, SyntheticCapturer, SystemCapturer, TILE_SIZE,
};
pub use ft_server::{FtServer, VirtualDrives, encode_win32_find_data_w};
pub use input_inject::{
    HostInputEvent, InputInjector, LOCAL_PRIORITY_COOLDOWN, LocalActivityGuard, NativeKey,
    PointerButtons, RecordingInjector, SystemInputInjector,
};
pub use session::{HostConfig, HostSession, ZrleEncoder, cli_host, serve_stream};
pub use view::{HostStatus, HostView};
