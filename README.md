<p align="center">
  <img src="assets/logo.png" alt="Lucarna logo" width="160">
</p>

<h1 align="center">Lucarna</h1>

<p align="center">Free, open-source VNC viewer for macOS, Windows and Linux, aiming at UltraVNC-compatible file transfer.</p>

Encodings: ZRLE, Zlib, Hextile, CopyRect, Raw. MS-Logon (Windows user and password) and VNC password login. Saved connections on the start dashboard, with passwords kept in the OS keychain (never on disk). Local cursor (no lag). Session bar: view-only, fit or 1:1 scaling, full screen (⌘⇧F), special keys menu. UltraVNC file transfer: two-pane local/remote browser with downloads, uploads, drag-and-drop queue, folder creation, rename and delete. Roadmap: [docs/ROADMAP.md](docs/ROADMAP.md).

The PNG is rendered from the SVG source; after editing `assets/logo.svg`, run:

```bash
rsvg-convert -w 512 -h 512 assets/logo.svg -o assets/logo.png
```

```bash
cargo run --release
```

## Architecture

Vertical slices, one per feature, named in the domain's language (RFB, RFC 6143):

```
src/
  main.rs            entry point
  app.rs             UI shell: connect form <-> live session
  session/           Session aggregate: owns the socket, routes server messages to features
  shared/wire.rs     big-endian wire primitives
  features/
    connection/      ServerAddress, handshake, VNC auth, saved connections, dashboard
    remote_screen/   Framebuffer, encodings, screen view
    input/           keysyms, key/pointer events
    clipboard/       cut text both ways
    file_transfer/   UltraVNC file transfer: two-pane browser, downloads, uploads, commands
```

Rules:
- A feature owns its domain types, its wire messages (`protocol.rs`) and its UI (`view.rs`).
- Features expose only what `mod.rs` re-exports. Cross-feature use goes through those exports.
- `session` is the only place that reads the socket; add a new server message by adding a match arm there that calls into your feature.
- Anything a malicious server sends is validated before use (sizes, rectangles, lengths).

## Tests

```bash
cargo test
docker build -t lucarna-test-server tests/server
docker run -d --rm --name lucarna-test -p 5999:5900 lucarna-test-server
VNC_TEST_ADDR=127.0.0.1:5999 VNC_TEST_PASSWORD=secret cargo test -- --ignored
```

## License

GPL-2.0-or-later
