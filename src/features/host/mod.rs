//! Embedded RFB Host (`src/features/host/`): user-space screen capture, 64×64 dirty-tile
//! ZRLE encoding, input injection with local mouse priority, and RFB 3.8 server session loop.

#![allow(dead_code, unused_imports)]

pub mod capture;
pub mod input_inject;
pub mod session;

pub use capture::{
    DamageTracker, DirtyRect, Frame, ScreenCapturer, SyntheticCapturer, SystemCapturer, TILE_SIZE,
};
pub use input_inject::{
    HostInputEvent, InputInjector, LOCAL_PRIORITY_COOLDOWN, LocalActivityGuard, NativeKey,
    PointerButtons, RecordingInjector, SystemInputInjector,
};
pub use session::{HostConfig, HostSession, ZrleEncoder, serve_stream};
