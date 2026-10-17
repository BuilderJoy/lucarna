# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Lucarna: cross-platform VNC viewer (Rust 2024, eframe/egui), targeting UltraVNC-compatible file transfer. Dependencies are deliberately minimal (`eframe` with `persistence`, `des` for VNC auth, `flate2` for zlib encodings, `keyring` for saved passwords); prefer std over new crates.

## Commands

```bash
cargo run --release                      # launch viewer
cargo test                               # unit tests
cargo test copy_rect_overlap             # single test by name
cargo clippy && cargo fmt

# Live integration test (ignored by default) against x11vnc in Docker:
docker build -t lucarna-test-server tests/server
docker run -d --rm --name lucarna-test -p 5999:5900 lucarna-test-server
VNC_TEST_ADDR=127.0.0.1:5999 VNC_TEST_PASSWORD=secret cargo test -- --ignored
```

Tests live in `#[cfg(test)]` modules inside `src/`; `tests/server/` only holds the Dockerfile.

## Architecture

Vertical slices per feature, named in RFB (RFC 6143) domain language: `src/features/{connection,remote_screen,input,clipboard}`.

- A feature owns its domain types, wire messages (`protocol.rs`) and UI (`view.rs`). Only what its `mod.rs` re-exports is public; cross-feature use goes through those exports.
- `shared/wire.rs`: big-endian read helpers and `invalid()` for `io::ErrorKind::InvalidData` errors.

### Threading / data flow

- `app.rs` (UI thread) spawns `session::run` on a background thread per connection and receives `session::Event`s over an `mpsc` channel, polled each frame; the session calls `ctx.request_repaint()` via `notify` after sending.
- `session/mod.rs` is the **only** socket reader. It performs handshake → `remote_screen::negotiate` → initial update request, then loops on the server message-type byte. **To add a server message, add a match arm there** that delegates to the feature.
- Writes go through `Outbound` (`Arc<Mutex<TcpStream>>` clone), shared by UI and reader thread. `Outbound::close()` shuts the socket, which ends the reader loop and yields `Event::Closed`.
- The `Framebuffer` is `Arc<Mutex<_>>`, written by the session thread, read by `ScreenView`; `dirty` flag signals texture re-upload.

### Protocol specifics

- Pixel format is forced to 32bpp BGRX true colour in `negotiate`; supported encodings: ZRLE, Zlib, Hextile, CopyRect, Raw, plus pseudo LastRect and DesktopSize. Unsupported encoding = error, so add new encodings to both the `negotiate` list and `read_update`. Zlib streams span the whole session, so their state lives in `remote_screen::Decoder`, created once in `session::run`.
- Security types: None (1) and VNC auth (2) only; RFB 3.3 legacy path handled separately in `handshake.rs`.
- Clipboard text is Latin-1 on the wire. `clipboard::forward_paste` must run before `input.forward` in `app.rs` so paste reaches the server before Ctrl+V.

Planned work and its order: [docs/ROADMAP.md](docs/ROADMAP.md).

### File transfer (UltraVNC, message type 7)

Protocol notes in `docs/ultravnc-ft.md`. `contentParam` is **little-endian** on the wire (UltraVNC never swaps it); `size`/`length` are big-endian. `RemoteFiles` (UI handle, in `Event::Connected`) sends requests; `file_transfer::Receiver` lives on the session thread, assembles listings and writes downloads to `~/Downloads` via a `.part` file. A FileHeader is only accepted if the UI requested that file. The server answers nothing when FT is disabled, so the UI times out.

### Saved connections

`connection/saved.rs`: names and addresses in `connections.txt` under `eframe::storage_dir("Lucarna")`; passwords **only** in the OS keychain (`keyring`, service `Lucarna`, account = normalized address). Never write a password to disk. `connection::Dashboard` is the start screen.

### Security rule

Treat the server as untrusted: validate every size, length and rectangle before allocating or indexing (see `MAX_DIM`, `MAX_TEXT`, `read_string` max, `Framebuffer::contains` checks).
