# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Lucarna: cross-platform VNC viewer (Rust 2024, eframe/egui), targeting UltraVNC-compatible file transfer. Dependencies are deliberately minimal (`eframe` with `persistence`, `des` for VNC auth, `flate2` for zlib encodings, `keyring` for saved passwords, `lucide-icons` for embedded vector UI icons); prefer std over new crates.

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

VS Code can keep showing old rust-analyzer errors after files change outside the editor (arguments or match arms "missing" that the code already has). `cargo check --all-targets` is the truth; to refresh the panel, save any `.rs` file or run "rust-analyzer: Restart server". From a terminal, adding a blank line to `Cargo.toml` and then `git checkout Cargo.toml` forces a workspace reload and a fresh check (touching `.rs` files does not).

Tests live in `#[cfg(test)]` modules inside `src/`; `tests/server/` holds the x11vnc Dockerfile and `colors.sh` (colour ramps so Tight uses JPEG and its filters); `tests/fixtures/` holds binary test inputs such as the Tight JPEG.

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

- Pixel format is forced to 32bpp BGRX true colour in `negotiate`; supported encodings: Tight (preferred, with JPEG quality 8), ZRLE, Zlib, Hextile, CopyRect, Raw, plus pseudo LastRect, DesktopSize and Cursor. Tight and ZRLE colours differ: Tight TPIXEL is R,G,B; ZRLE CPIXEL is the low 3 bytes of our little-endian pixel (B,G,R). A Cursor rect carries the hotspot in x/y (not a position), so it is handled before the bounds check; the shape lands in `Framebuffer::cursor` and `ScreenView::draw_cursor` paints it at the local pointer, hiding the OS cursor only once a shape has arrived. Unsupported encoding = error, so add new encodings to both the `negotiate` list and `read_update`. Zlib streams span the whole session, so their state lives in `remote_screen::Decoder`, created once in `session::run`.
- Security types: None (1), VNC auth (2), MS-Logon II (0x71) and III (0x76); RFB 3.3 legacy path (including `0xFFFFFFFA` = MS-Logon II) handled in `handshake.rs`. `pick` chooses by whether a user name was given and never answers the UltraVNC marker (17). Details and the weak-crypto caveat in `docs/ultravnc-auth.md`. Credentials travel as `connection::Credentials` (user may be empty); the user name is saved in `connections.txt`, the password only in the keychain.
- Clipboard: `clipboard::RemoteClipboard` advertises `ENC_EXTENDED_CLIPBOARD` (`0xC0A1E5CE`) in `SetEncodings`. When the server negotiates Extended Clipboard (`length < 0` with `CLIP_CAPS`), text is exchanged as zlib-compressed UTF-8 (`CLIP_PROVIDE | CLIP_UTF8`, `CLIP_NOTIFY`, `CLIP_REQUEST`, `CLIP_PEEK`); otherwise it falls back to classic RFB Latin-1. `RemoteClipboard::forward_paste` must run before `input.forward` in `app.rs` so paste reaches the server before Ctrl+V.

Planned work and its order: [docs/ROADMAP.md](docs/ROADMAP.md).

## Git workflow

`main` holds releases (milestones such as v0.1); `develop` is the integration branch and nobody commits to it directly. **Every new feature, fix or chore gets its own branch cut from an up-to-date `develop`**, named `feature/<topic>` (e.g. `feature/tight-encoding`), `fix/<topic>`, `chore/<topic>` or `docs/<topic>`. Commit there, updating `docs/ROADMAP.md` (✅, date, deviations) and this file when architecture changes, and push the branch after each commit. When `cargo test` and `cargo clippy --all-targets` are clean, open a PR into `develop` (`gh pr create --base develop`) and leave the merge to the user; never merge your own PR unasked. A release is a PR from `develop` into `main`. Other sessions commit in parallel: `git fetch` and read `git log origin/develop` before starting and before editing files you haven't touched recently. Commit trailer: `Co-Authored-By: <model in use>`.

### Chat and repeater

`features/chat/`: TextChat (message 11), `session::run` forwards `Event::Chat`, `ChatView` is the window. The server can open it, so `app.rs` also includes `chat.rect` in the pointer-blocking list. Repeater: `ConnectRequest.repeater` → `session::run(.., repeater, ..)` dials the repeater and calls `handshake::via_repeater(address)` before `handshake::perform`. With a repeater, `address` is the target (`host:port` or `ID:1234`), not what we dial. Latin-1 helpers live in `shared/wire.rs` (`from_latin1`, `to_latin1`).

### Session bar

`app.rs::bar_contents` draws the bar: View only, Fit/1:1 (`remote_screen::Scale`), Full screen (⌘⇧F, consumed in `ui()` before `input.forward`), Keys menu (`input::SHORTCUTS`), Ctrl+Alt+Del, Chat, Files, Disconnect. In full screen the bar is an overlay `Area`; `bar_rect` and the files window rect stop pointer events reaching the server, and `egui_wants_keyboard_input()` stops keyboard events while typing in our own UI text inputs.

### File transfer (UltraVNC, message type 7)

Protocol notes in `docs/ultravnc-ft.md`. `contentParam` is **little-endian** on the wire (UltraVNC never swaps it); `size`/`length` are big-endian. Both `FileHeader` (4) and `FileTransferOffer` (8) carry 4 extra trailing bytes (`sizeH`, high 32 bits of file size) outside `length`. `RemoteFiles` (UI handle, in `Event::Connected`) sends requests and spawns background upload streaming when the server answers `FileAcceptHeader`; `file_transfer::Receiver` lives on the session thread, tracks negotiated block size (`8 KiB` default for `libvncserver`, `32 KiB` when UltraVNC sends `FileTransferProtocolVersion` v3+), assembles listings, and writes downloads to the active local directory via a `.lucarna-part` file. `FilesView` is a two-pane (`Local` | `Remote`) browser supporting drag-and-drop upload queues and remote commands (`CDirCreate`, `CFileDelete`, `CFileRename`).

### Saved connections & Address Book

`connection/saved.rs`: names, addresses, usernames, labels, favorites, per-connection `SessionOptions` and `last_connected` timestamps in `connections.txt` under `eframe::storage_dir("Lucarna")` (backward-compatible with 2- and 3-column files); compressed RGBA desktop preview thumbnails in `previews/<address>.lcpv` (`Framebuffer::thumbnail`); passwords **only** in the OS keychain (`keyring`, service `Lucarna`, account = normalized address). Never write a password to disk. `connection::Dashboard` is the RealVNC-style Address Book start screen with sidebar label filtering, grid/list views, context menus, keyboard shortcuts and the per-connection `Properties` dialog.

### Security rule

Treat the server as untrusted: validate every size, length and rectangle before allocating or indexing (see `MAX_DIM`, `MAX_TEXT`, `read_string` max, `Framebuffer::contains` checks).
