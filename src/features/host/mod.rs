//! Sharing this computer: Lucarna as the RFB 3.8 **server** ("host"). Everything else in the app is the viewer.
//!
//! # Map of the files
//!
//! | File | What it holds |
//! |---|---|
//! | `view.rs` | The "Share this device" page, the strips across the top of the window, and the two background loops that wait for a viewer (`run_listener_host_loop`, `run_repeater_host_loop`). |
//! | `session.rs` | One connection: the RFB handshake, the message loop (`HostSession::step`), the ZRLE encoder and `HostConfig`. |
//! | `capture.rs` | `Frame` (one picture of the screen, 4 bytes per pixel: blue, green, red, unused), `DamageTracker` (which 64×64 tiles changed) and `SystemCapturer`. |
//! | `os_input.rs` | The part that differs per operating system: move the pointer, press keys, read the cursor, check permissions, and `Grabber` for capture. |
//! | `mac_capture.rs`, `x11_capture.rs` | Fast capture on macOS (ScreenCaptureKit stream) and Linux (X server). |
//! | `input_inject.rs` | Turns RFB key and pointer events into `os_input` calls. Holds the rule that the person at the computer wins over the remote mouse (`LocalActivityGuard`). |
//! | `ft_server.rs` | File transfer (RFB message 7): the shared folders, limits, cancel, and the events shown to the person sharing. |
//!
//! # Threads
//!
//! - **UI thread**: draws `HostView`. It never touches a socket.
//! - **Host thread**: started by `HostView` when sharing begins. It runs one of the two loops in `view.rs`,
//!   accepts a viewer and then stays inside `serve_stream` until that viewer leaves.
//! - They talk in two ways: the host thread sends `HostUiEvent`s over a channel (connected, file received,
//!   wrong PIN), and both share a few atomic values (`cancel`, `mouse_blocked`, `HostPerf`).
//! - On macOS the system also calls `mac_capture` on its own queue each time a new picture is ready.
//!
//! # What happens when a viewer connects
//!
//! 1. The loop in `view.rs` gets a socket, either by listening on a port or through a repeater.
//! 2. With a PIN: `shared::crypto::host_e2ee_handshake` (SPAKE2). A wrong PIN is counted; three replace the PIN.
//!    From here on every byte is encrypted (`E2eeStream`).
//! 3. `HostSession::handshake`: RFB version, security type, then the **Allow / Deny** question
//!    (`HostConfig::gate`, answered by the person sharing), then `ServerInit` with the screen size.
//! 4. `serve_stream` calls `HostSession::step` in a loop. Each call reads one message from the viewer.
//!
//! # The path of a picture
//!
//! viewer sends `FramebufferUpdateRequest` → `step` → `send_framebuffer_update` →
//! `SystemCapturer::capture` (→ `os_input::Grabber`) → `DamageTracker::diff` gives the changed rectangles →
//! `ZrleEncoder::encode_rect` compresses each one → one `FramebufferUpdate` message goes out.
//!
//! # The path of a key or mouse event
//!
//! viewer sends `KeyEvent` / `PointerEvent` → `step` → `InputInjector::inject` → dropped when the session is
//! view-only or the local person moved the mouse in the last 2 s → `os_input::{pointer_to, button, key, wheel}`.
//!
//! # Tests
//!
//! `SyntheticCapturer` and `RecordingInjector` stand in for the real screen and the real mouse, so a test
//! never captures or moves anything (`SystemInputInjector::real` is false in tests).

#![allow(dead_code, unused_imports)]

pub mod capture;
pub mod ft_server;
pub mod input_inject;
#[cfg(target_os = "macos")]
mod mac_capture;
pub mod os_input;
pub mod session;
pub mod view;
#[cfg(all(unix, not(target_os = "macos")))]
mod x11_capture;

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
