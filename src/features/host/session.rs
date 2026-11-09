//! Embedded RFB 3.8 Host session loop and session-persistent ZRLE tile encoder.

use std::io::{self, Read, Write};

use flate2::{Compress, Compression, FlushCompress};

use super::capture::{DamageTracker, DirtyRect, ScreenCapturer, SystemCapturer, TILE_SIZE};
use super::ft_server::{FtServer, VirtualDrives};
use super::input_inject::{HostInputEvent, InputInjector, SystemInputInjector};
use crate::features::connection::vnc_response;
use crate::features::file_transfer::ENC_PROTOCOL_VERSION;
use crate::shared::wire::{from_latin1, invalid, read_bytes, read_u8, read_u16, read_u32};

const SEC_NONE: u8 = 1;
const SEC_VNC_AUTH: u8 = 2;

const ENC_RAW: i32 = 0;
const ENC_ZRLE: i32 = 16;
const ENC_DESKTOP_SIZE: i32 = -223;

const MAX_ENCODINGS: usize = 256;
const MAX_CUT_TEXT: usize = 1 << 20;
const ZRLE_TILE: usize = TILE_SIZE as usize;

/// Server-side session-persistent ZRLE encoder (RFC 6143 §7.7.6).
pub struct ZrleEncoder {
    zlib: Compress,
}

impl Default for ZrleEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZrleEncoder {
    pub fn new() -> Self {
        Self {
            // Fast compression (level 1-3) keeps CPU overhead negligible for live audio/DJ hosts.
            zlib: Compress::new(Compression::fast(), true),
        }
    }

    /// Encodes a `w × h` BGRX rectangle into ZRLE tiles and deflates it into the persistent zlib stream.
    pub fn encode_rect(&mut self, pixels: &[u8], w: usize, h: usize) -> io::Result<Vec<u8>> {
        if pixels.len() != w * h * 4 {
            return Err(invalid("ZRLE input buffer size mismatch"));
        }
        let mut raw_tiles = Vec::with_capacity(w * h);
        for ty in (0..h).step_by(ZRLE_TILE) {
            let th = ZRLE_TILE.min(h - ty);
            for tx in (0..w).step_by(ZRLE_TILE) {
                let tw = ZRLE_TILE.min(w - tx);
                encode_tile(&mut raw_tiles, pixels, w, tx, ty, tw, th);
            }
        }
        deflate_sync(&mut self.zlib, &raw_tiles)
    }
}

fn encode_tile(
    out: &mut Vec<u8>,
    pixels: &[u8],
    stride_px: usize,
    tx: usize,
    ty: usize,
    tw: usize,
    th: usize,
) {
    let count = tw * th;
    let mut cpixels = Vec::with_capacity(count);
    for r in 0..th {
        let row_start = ((ty + r) * stride_px + tx) * 4;
        for c in 0..tw {
            let p = &pixels[row_start + c * 4..row_start + c * 4 + 3];
            cpixels.push([p[0], p[1], p[2]]);
        }
    }

    let first = cpixels[0];
    if cpixels.iter().all(|&c| c == first) {
        // Subencoding 1: Solid tile (1 subencoding byte + 3 CPIXEL bytes).
        out.push(1);
        out.extend_from_slice(&first);
        return;
    }

    // Build Plain RLE (subencoding 128) and compare against Raw (subencoding 0).
    let mut rle = Vec::with_capacity(count * 3);
    rle.push(128);
    let mut i = 0;
    while i < count {
        let c = cpixels[i];
        let mut run = 1usize;
        while i + run < count && cpixels[i + run] == c {
            run += 1;
        }
        rle.extend_from_slice(&c);
        let mut rem = run - 1;
        while rem >= 255 {
            rle.push(255);
            rem -= 255;
        }
        rle.push(rem as u8);
        i += run;
    }

    let raw_len = 1 + count * 3;
    if rle.len() < raw_len {
        out.extend_from_slice(&rle);
    } else {
        out.push(0);
        for c in cpixels {
            out.extend_from_slice(&c);
        }
    }
}

fn deflate_sync(z: &mut Compress, input: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 2 + 64);
    let mut pos = 0;
    loop {
        out.reserve(16 << 10);
        let (before_in, before_out) = (z.total_in(), z.total_out());
        z.compress_vec(&input[pos..], &mut out, FlushCompress::Sync)
            .map_err(|e| invalid(e.to_string()))?;
        pos += (z.total_in() - before_in) as usize;
        if pos == input.len() && out.len() < out.capacity() {
            return Ok(out);
        }
        if (z.total_in(), z.total_out()) == (before_in, before_out) {
            return Err(invalid("zlib compression stream stalled"));
        }
    }
}

/// Configuration for an embedded RFB Host session.
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub desktop_name: String,
    pub password: String,
    pub view_only: bool,
    pub fixed_challenge: Option<[u8; 16]>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            desktop_name: "Lucarna Studio Host".into(),
            password: String::new(),
            view_only: false,
            fixed_challenge: None,
        }
    }
}

/// Active RFB 3.8 Host session state.
pub struct HostSession<C: ScreenCapturer, I: InputInjector> {
    config: HostConfig,
    pub capturer: C,
    pub injector: I,
    pub ft_server: FtServer,
    damage: DamageTracker,
    zrle: ZrleEncoder,
    use_zrle: bool,
    supports_desktop_size: bool,
    screen_size: (u16, u16),
    pub client_clipboard: Option<String>,
}

impl<C: ScreenCapturer, I: InputInjector> HostSession<C, I> {
    pub fn new(config: HostConfig, capturer: C, injector: I) -> Self {
        Self::with_drives(config, capturer, injector, VirtualDrives::default())
    }

    pub fn with_drives(
        config: HostConfig,
        capturer: C,
        injector: I,
        drives: VirtualDrives,
    ) -> Self {
        let read_only = config.view_only;
        Self {
            config,
            capturer,
            injector,
            ft_server: FtServer::new(drives, read_only),
            damage: DamageTracker::new(),
            zrle: ZrleEncoder::new(),
            use_zrle: false,
            supports_desktop_size: false,
            screen_size: (0, 0),
            client_clipboard: None,
        }
    }

    /// Performs the server side of the RFB 3.8 handshake (version, security, ClientInit, ServerInit).
    pub fn handshake(&mut self, s: &mut (impl Read + Write)) -> io::Result<()> {
        s.write_all(b"RFB 003.008\n")?;
        s.flush()?;

        let ver = read_bytes(s, 12)?;
        if &ver[..4] != b"RFB " {
            return Err(invalid("client did not send an RFB version"));
        }

        let sec_type = if self.config.password.is_empty() {
            SEC_NONE
        } else {
            SEC_VNC_AUTH
        };
        s.write_all(&[1, sec_type])?;
        s.flush()?;

        let chosen = read_u8(s)?;
        if chosen != sec_type {
            write_auth_failure(s, "unsupported security type selected")?;
            return Err(invalid("client selected unsupported security type"));
        }

        if sec_type == SEC_VNC_AUTH {
            let mut challenge = [0u8; 16];
            match self.config.fixed_challenge {
                Some(c) => challenge = c,
                None => {
                    getrandom::fill(&mut challenge).map_err(|e| invalid(e.to_string()))?;
                }
            }
            s.write_all(&challenge)?;
            s.flush()?;

            let response: [u8; 16] = read_bytes(s, 16)?.try_into().unwrap();
            let expected = vnc_response(&self.config.password, &challenge);
            if !constant_time_eq(&response, &expected) {
                write_auth_failure(s, "invalid password or PIN")?;
                return Err(invalid("client failed VNC authentication"));
            }
        }

        // SecurityResult = 0 (OK).
        s.write_all(&0u32.to_be_bytes())?;
        s.flush()?;

        // ClientInit (shared flag).
        let _shared = read_u8(s)?;

        // Capture initial frame to advertise accurate desktop geometry in ServerInit.
        let initial = self.capturer.capture()?;
        self.screen_size = (initial.width, initial.height);
        self.damage.mark_full_dirty();

        let mut init = Vec::with_capacity(24 + self.config.desktop_name.len());
        init.extend_from_slice(&initial.width.to_be_bytes());
        init.extend_from_slice(&initial.height.to_be_bytes());
        // 32bpp, depth 24, little-endian, true-colour, max 255, shifts R=16 G=8 B=0 (BGRX).
        #[rustfmt::skip]
        init.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        let name_bytes = self.config.desktop_name.as_bytes();
        init.extend_from_slice(&(name_bytes.len() as u32).to_be_bytes());
        init.extend_from_slice(name_bytes);
        s.write_all(&init)?;
        s.flush()
    }

    /// Reads and processes a single client-to-server RFB message.
    pub fn step(&mut self, s: &mut (impl Read + Write)) -> io::Result<()> {
        match read_u8(s)? {
            0 => {
                // SetPixelFormat: 3 padding bytes + 16-byte PIXEL_FORMAT.
                read_bytes(s, 19)?;
                Ok(())
            }
            2 => {
                // SetEncodings: 1 padding byte + u16 number-of-encodings + i32 encodings.
                read_u8(s)?;
                let count = usize::from(read_u16(s)?);
                if count > MAX_ENCODINGS {
                    return Err(invalid("too many encodings in SetEncodings"));
                }
                self.use_zrle = false;
                self.supports_desktop_size = false;
                let mut wants_ft_v4 = false;
                for _ in 0..count {
                    match read_u32(s)? as i32 {
                        ENC_ZRLE => self.use_zrle = true,
                        ENC_DESKTOP_SIZE => self.supports_desktop_size = true,
                        ENC_PROTOCOL_VERSION => wants_ft_v4 = true,
                        _ => {}
                    }
                }
                if wants_ft_v4 {
                    self.ft_server.send_protocol_version(s)?;
                }
                Ok(())
            }
            3 => {
                // FramebufferUpdateRequest: incremental(u8), x(u16), y(u16), w(u16), h(u16).
                let incremental = read_u8(s)? != 0;
                let _req_rect = [read_u16(s)?, read_u16(s)?, read_u16(s)?, read_u16(s)?];
                if !incremental {
                    self.damage.mark_full_dirty();
                }
                self.send_framebuffer_update(s)
            }
            4 => {
                // KeyEvent: down-flag(u8), 2 padding bytes, keysym(u32).
                let down = read_u8(s)? != 0;
                read_u16(s)?;
                let keysym = read_u32(s)?;
                if !self.config.view_only {
                    self.injector.inject(HostInputEvent::Key { down, keysym })?;
                }
                Ok(())
            }
            5 => {
                // PointerEvent: button-mask(u8), x(u16), y(u16).
                let buttons = read_u8(s)?;
                let x = read_u16(s)?;
                let y = read_u16(s)?;
                if !self.config.view_only {
                    self.injector
                        .inject(HostInputEvent::Pointer { buttons, x, y })?;
                }
                Ok(())
            }
            6 => {
                // ClientCutText: 3 padding bytes + u32 length + Latin-1 text.
                read_bytes(s, 3)?;
                let len = read_u32(s)? as usize;
                if len > MAX_CUT_TEXT {
                    return Err(invalid("client cut text too large"));
                }
                let bytes = read_bytes(s, len)?;
                self.client_clipboard = Some(from_latin1(&bytes));
                Ok(())
            }
            7 => self.ft_server.handle_message(s),
            msg => Err(invalid(format!("unsupported client message type {msg}"))),
        }
    }

    fn send_framebuffer_update(&mut self, s: &mut impl Write) -> io::Result<()> {
        let frame = self.capturer.capture()?;
        let resized = (frame.width, frame.height) != self.screen_size;
        self.screen_size = (frame.width, frame.height);

        let mut rects = self.damage.diff(&frame);
        // RFB requires every FramebufferUpdateRequest to eventually receive a response;
        // if nothing changed, pace at ~60 Hz and send a 1×1 top-left refresh so lock-step viewers keep polling.
        if rects.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(16));
            rects.push(DirtyRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            });
        }

        let extra_resize = usize::from(resized && self.supports_desktop_size);
        let total_rects = (rects.len() + extra_resize) as u16;

        let mut msg = Vec::new();
        msg.extend_from_slice(&[0, 0]); // message-type 0 + 1 byte padding
        msg.extend_from_slice(&total_rects.to_be_bytes());

        if resized && self.supports_desktop_size {
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&frame.width.to_be_bytes());
            msg.extend_from_slice(&frame.height.to_be_bytes());
            msg.extend_from_slice(&ENC_DESKTOP_SIZE.to_be_bytes());
        }

        for r in rects {
            msg.extend_from_slice(&r.x.to_be_bytes());
            msg.extend_from_slice(&r.y.to_be_bytes());
            msg.extend_from_slice(&r.w.to_be_bytes());
            msg.extend_from_slice(&r.h.to_be_bytes());
            let region = frame.region(r);
            if self.use_zrle {
                msg.extend_from_slice(&ENC_ZRLE.to_be_bytes());
                let compressed =
                    self.zrle
                        .encode_rect(&region, usize::from(r.w), usize::from(r.h))?;
                msg.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
                msg.extend_from_slice(&compressed);
            } else {
                msg.extend_from_slice(&ENC_RAW.to_be_bytes());
                msg.extend_from_slice(&region);
            }
        }

        s.write_all(&msg)?;
        s.flush()
    }
}

fn write_auth_failure(s: &mut impl Write, reason: &str) -> io::Result<()> {
    s.write_all(&1u32.to_be_bytes())?;
    s.write_all(&(reason.len() as u32).to_be_bytes())?;
    s.write_all(reason.as_bytes())?;
    s.flush()
}

fn constant_time_eq(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Runs a complete user-space RFB 3.8 Host session over `stream` using the OS screen capturer and input injector.
pub fn serve_stream(config: HostConfig, stream: &mut (impl Read + Write)) -> io::Result<()> {
    let view_only = config.view_only;
    let mut host = HostSession::new(
        config,
        SystemCapturer::new((1280, 720)),
        SystemInputInjector::new(view_only),
    );
    host.handshake(stream)?;
    loop {
        match host.step(stream) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// Performs the E2EE handshake with PIN authentication and serves the RFB 3.8 Host session inside the AEAD tunnel.
pub fn serve_e2ee_stream(
    config: HostConfig,
    stream: &mut (impl Read + Write),
    pin: &str,
    limiter: &mut crate::shared::crypto::BruteForceLimiter,
) -> io::Result<()> {
    let keys = crate::shared::crypto::host_e2ee_handshake(stream, pin, limiter)?;
    let mut e2ee = crate::shared::crypto::E2eeStream::new(stream, keys);
    serve_stream(config, &mut e2ee)
}

/// Registers this Host with an UltraVNC Repeater in Mode II using a 250-byte NUL-padded `ID:XXXXXX` header.
pub fn register_repeater_mode_ii(stream: &mut impl Write, session_code: &str) -> io::Result<()> {
    let clean = crate::shared::crypto::normalize_code(session_code);
    if clean.is_empty() || clean.len() > 240 {
        return Err(invalid("invalid session code for repeater Mode II"));
    }
    let id_header = format!("ID:{clean}");
    let mut msg = [0u8; 250];
    msg[..id_header.len()].copy_from_slice(id_header.as_bytes());
    stream.write_all(&msg)?;
    stream.flush()
}

/// Checks if Lucarna was launched with `--host [bind_addr] [--password <pw>] [--pin <pin>] [--repeater <addr>] [--code <code>] [--view-only]`
/// and runs the embedded RFB 3.8 + Native File Transfer + E2EE Host in user space.
pub fn cli_host() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "--host") {
        return None;
    }

    let mut bind_addr = String::from("127.0.0.1:5950");
    let mut password = String::from("studio");
    let mut pin = String::new();
    let mut repeater_addr = String::new();
    let mut session_code = String::new();
    let mut view_only = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                if let Some(next) = args.get(i + 1)
                    && !next.starts_with("--")
                {
                    bind_addr = next.clone();
                    i += 1;
                }
            }
            "--password" => {
                if let Some(next) = args.get(i + 1) {
                    password = next.clone();
                    i += 1;
                }
            }
            "--pin" => {
                if let Some(next) = args.get(i + 1) {
                    pin = next.clone();
                    i += 1;
                }
            }
            "--repeater" => {
                if let Some(next) = args.get(i + 1) {
                    repeater_addr = next.clone();
                    i += 1;
                }
            }
            "--code" => {
                if let Some(next) = args.get(i + 1) {
                    session_code = crate::shared::crypto::normalize_code(next);
                    i += 1;
                }
            }
            "--view-only" => view_only = true,
            _ => {}
        }
        i += 1;
    }

    if !repeater_addr.is_empty() {
        if session_code.is_empty() {
            session_code = crate::shared::crypto::generate_session_code();
        }
        if pin.is_empty() {
            pin = crate::shared::crypto::generate_pin();
        }
        let formatted_code = crate::shared::crypto::format_code(&session_code);
        println!("================================================================");
        println!("  Lucarna QuickSupport Host (Fase 3: Repetidor Modo II + E2EE)");
        println!("================================================================");
        println!("  Repetidor     : {repeater_addr}");
        println!("  Código Sesión : {formatted_code} (ID:{session_code})");
        println!("  PIN E2EE      : {pin} (Bloqueo tras 3 intentos fallidos)");
        println!("================================================================");

        let config = HostConfig {
            desktop_name: "Lucarna QuickSupport Host".into(),
            password: String::new(),
            view_only,
            fixed_challenge: None,
        };
        let mut limiter = crate::shared::crypto::BruteForceLimiter::new();
        while !limiter.is_locked() {
            match std::net::TcpStream::connect(&repeater_addr) {
                Ok(mut stream) => {
                    let _ = stream.set_nodelay(true);
                    if let Err(e) = register_repeater_mode_ii(&mut stream, &session_code) {
                        eprintln!("[!] Error al registrar ID en repetidor: {e}");
                        return Some(1);
                    }
                    println!(
                        "[+] Registrado en repetidor como ID:{session_code}. Esperando operador..."
                    );
                    match serve_e2ee_stream(config.clone(), &mut stream, &pin, &mut limiter) {
                        Ok(()) => {
                            println!("[-] Sesión E2EE finalizada correctamente.");
                            break;
                        }
                        Err(e) => {
                            eprintln!("[!] Sesión E2EE rechazada o cerrada: {e}");
                            if limiter.is_locked() {
                                eprintln!(
                                    "[X] ALERTA DE SEGURIDAD: 3 intentos de PIN fallidos. Código {formatted_code} revocado."
                                );
                                return Some(2);
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[!] No se pudo conectar al repetidor {repeater_addr}: {e}");
                    return Some(1);
                }
            }
        }
        return Some(0);
    }

    let listener = match std::net::TcpListener::bind(&bind_addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Error al abrir el puerto Host en {bind_addr}: {e}");
            return Some(1);
        }
    };

    let drives = VirtualDrives::default();
    println!("================================================================");
    println!("  Lucarna Studio Host (RFB 3.8 + Archivos sin SSH + E2EE)");
    println!("================================================================");
    println!("  Escuchando en : {bind_addr}");
    if !pin.is_empty() {
        println!("  Túnel E2EE    : ACTIVO (ChaCha20-Poly1305 + X25519, PIN: {pin})");
    } else {
        println!(
            "  Contraseña    : {}",
            if password.is_empty() {
                "(sin contraseña)"
            } else {
                &password
            }
        );
    }
    println!(
        "  Modo          : {}",
        if view_only {
            "Solo lectura / Solo ver"
        } else {
            "Control interactivo + Transferencia de archivos"
        }
    );
    println!("  Unidades Virtuales Compartidas (Sandbox canonicalize):");
    for (letter, path) in drives.roots() {
        println!("    {letter}:\\  ->  {}", path.display());
    }
    println!("================================================================");

    let config = HostConfig {
        desktop_name: "Lucarna Studio Host".into(),
        password: if pin.is_empty() {
            password
        } else {
            String::new()
        },
        view_only,
        fixed_challenge: None,
    };
    let mut limiter = crate::shared::crypto::BruteForceLimiter::new();

    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                let peer = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| "cliente".into());
                let _ = stream.set_nodelay(true);
                println!("[+] Visor conectado desde {peer}");
                let res = if !pin.is_empty() {
                    serve_e2ee_stream(config.clone(), &mut stream, &pin, &mut limiter)
                } else {
                    serve_stream(config.clone(), &mut stream)
                };
                match res {
                    Ok(()) => println!("[-] Sesión finalizada con {peer}"),
                    Err(e) => {
                        println!("[!] Sesión cerrada con {peer}: {e}");
                        if limiter.is_locked() {
                            eprintln!(
                                "[X] 3 intentos de PIN fallidos. Cerrando Host por seguridad."
                            );
                            return Some(2);
                        }
                    }
                }
            }
            Err(e) => eprintln!("[!] Error de conexión entrante: {e}"),
        }
    }
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::connection::{Credentials, handshake};
    use crate::features::host::capture::{Frame, SyntheticCapturer};
    use crate::features::host::input_inject::{NativeKey, RecordingInjector};
    use crate::features::remote_screen::{self, Decoder, Framebuffer};
    use std::collections::VecDeque;
    use std::sync::{Mutex, mpsc};
    use std::thread;

    struct PipeEnd {
        rx: mpsc::Receiver<Vec<u8>>,
        tx: mpsc::Sender<Vec<u8>>,
        buf: VecDeque<u8>,
    }

    fn pipe_pair() -> (PipeEnd, PipeEnd) {
        let (tx_a, rx_b) = mpsc::channel();
        let (tx_b, rx_a) = mpsc::channel();
        (
            PipeEnd {
                rx: rx_a,
                tx: tx_a,
                buf: VecDeque::new(),
            },
            PipeEnd {
                rx: rx_b,
                tx: tx_b,
                buf: VecDeque::new(),
            },
        )
    }

    impl Read for PipeEnd {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            while self.buf.is_empty() {
                match self.rx.recv() {
                    Ok(chunk) => self.buf.extend(chunk),
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len());
            for b in &mut out[..n] {
                *b = self.buf.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl Write for PipeEnd {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if !data.is_empty() {
                self.tx
                    .send(data.to_vec())
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))?;
            }
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn zrle_encoder_roundtrips_solid_and_patterned_rects_with_viewer_decoder() {
        let mut enc = ZrleEncoder::new();
        let mut dec = Decoder::default();

        // 130×70 frame: spans multiple 64×64 tiles with solid and gradient areas.
        let mut frame = Frame::solid(130, 70, [40, 80, 120, 0]);
        for x in 0..64 {
            frame.set_pixel(x, 0, [(x * 3) as u8, 10, 200, 0]);
        }

        let compressed = enc.encode_rect(&frame.pixels, 130, 70).unwrap();
        let mut wire = vec![0, 0, 1, 0, 0, 0, 0, 0, 130, 0, 70];
        wire.extend_from_slice(&ENC_ZRLE.to_be_bytes());
        wire.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        wire.extend_from_slice(&compressed);

        let screen = Mutex::new(Framebuffer::new(130, 70).unwrap());
        remote_screen::read_update(&mut &wire[..], &screen, &mut dec).unwrap();
        assert_eq!(screen.lock().unwrap().pixels, frame.pixels);
    }

    #[test]
    fn viewer_and_host_complete_full_rfb_session_over_stream() {
        let (mut server_stream, mut client) = pipe_pair();

        let mut f1 = Frame::solid(128, 64, [15, 30, 45, 0]);
        f1.set_pixel(10, 10, [200, 100, 50, 0]);
        let mut f2 = f1.clone();
        // Second frame modifies tile (1, 0) at x = 80.
        f2.set_pixel(80, 20, [99, 88, 77, 0]);

        let expected_f1 = f1.pixels.clone();
        let expected_f2 = f2.pixels.clone();

        let server_handle = thread::spawn(move || {
            let cfg = HostConfig {
                desktop_name: "Radio Studio A".into(),
                password: "djpin".into(),
                view_only: false,
                fixed_challenge: Some(*b"0123456789abcdef"),
            };
            let capturer = SyntheticCapturer::new(vec![f1.clone(), f1, f2]);
            let injector = RecordingInjector::new(false);
            let mut host = HostSession::new(cfg, capturer, injector);

            host.handshake(&mut server_stream).unwrap();
            // 1. SetPixelFormat, 2. SetEncodings, 3. Initial FramebufferUpdateRequest,
            // 4. Incremental FramebufferUpdateRequest, 5. KeyEvent, 6. PointerEvent.
            for _ in 0..6 {
                host.step(&mut server_stream).unwrap();
            }
            host.injector
        });

        let creds = Credentials {
            user: String::new(),
            password: "djpin".into(),
            pin: String::new(),
        };
        let init = handshake::perform(&mut client, &creds).unwrap();
        assert_eq!((init.width, init.height), (128, 64));
        assert_eq!(init.name, "Radio Studio A");

        remote_screen::negotiate(&mut client, &[ENC_PROTOCOL_VERSION]).unwrap();
        // Host immediately answers ENC_PROTOCOL_VERSION with FT ProtocolVersion v4 (32 KiB blocks).
        assert_eq!(read_u8(&mut client).unwrap(), 7);
        assert_eq!(
            crate::features::file_transfer::read(&mut client).unwrap(),
            crate::features::file_transfer::Msg::ProtocolVersion {
                version: 4,
                block_size: 32768,
            }
        );

        let screen = Mutex::new(Framebuffer::new(128, 64).unwrap());
        let mut decoder = Decoder::default();

        // Request full frame (incremental = 0).
        client
            .write_all(&[3, 0, 0, 0, 0, 0, 0, 128, 0, 64])
            .unwrap();
        assert_eq!(read_u8(&mut client).unwrap(), 0);
        remote_screen::read_update(&mut client, &screen, &mut decoder).unwrap();
        assert_eq!(screen.lock().unwrap().pixels, expected_f1);

        // Request incremental frame (incremental = 1): only modified tile (64..128, 0..64) travels via ZRLE.
        client
            .write_all(&[3, 1, 0, 0, 0, 0, 0, 128, 0, 64])
            .unwrap();
        assert_eq!(read_u8(&mut client).unwrap(), 0);
        remote_screen::read_update(&mut client, &screen, &mut decoder).unwrap();
        assert_eq!(screen.lock().unwrap().pixels, expected_f2);

        // Send KeyEvent ('A' = 0x41 down) and PointerEvent (left button at 42, 24).
        client.write_all(&[4, 1, 0, 0, 0, 0, 0, 0x41]).unwrap();
        client.write_all(&[5, 1, 0, 42, 0, 24]).unwrap();

        let recorded = server_handle.join().unwrap();
        assert_eq!(recorded.decoded_keys, vec![(true, NativeKey::Char('A'))]);
        assert_eq!(
            recorded.events,
            vec![
                HostInputEvent::Key {
                    down: true,
                    keysym: 0x41
                },
                HostInputEvent::Pointer {
                    buttons: 1,
                    x: 42,
                    y: 24
                },
            ]
        );
    }

    #[test]
    fn e2ee_repeater_mode_ii_session_roundtrip() {
        let (mut server_stream, mut client) = pipe_pair();
        let f1 = Frame::solid(64, 64, [20, 40, 60, 0]);
        let expected_f1 = f1.pixels.clone();

        let server_handle = thread::spawn(move || {
            // 1. Register Mode II ID on repeater
            register_repeater_mode_ii(&mut server_stream, "582-914").unwrap();

            // 2. Perform E2EE handshake with PIN "4920"
            let mut limiter = crate::shared::crypto::BruteForceLimiter::new();
            let keys = crate::shared::crypto::host_e2ee_handshake(
                &mut server_stream,
                "4920",
                &mut limiter,
            )
            .unwrap();
            let mut e2ee = crate::shared::crypto::E2eeStream::new(server_stream, keys);

            // 3. Serve RFB 3.8 session inside the AEAD tunnel
            let cfg = HostConfig {
                desktop_name: "Encrypted Studio".into(),
                password: String::new(),
                view_only: false,
                fixed_challenge: None,
            };
            let capturer = SyntheticCapturer::new(vec![f1]);
            let injector = RecordingInjector::new(false);
            let mut host = HostSession::new(cfg, capturer, injector);

            host.handshake(&mut e2ee).unwrap();
            // SetPixelFormat, SetEncodings, FramebufferUpdateRequest
            for _ in 0..3 {
                host.step(&mut e2ee).unwrap();
            }
        });

        // Verify the repeater sees the 250-byte "ID:582914\0..." registration packet
        let mut id_packet = [0u8; 250];
        client.read_exact(&mut id_packet).unwrap();
        assert_eq!(&id_packet[0..9], b"ID:582914");
        assert!(id_packet[9..].iter().all(|&b| b == 0));

        // Viewer performs E2EE handshake with matching PIN
        let v_keys = crate::shared::crypto::viewer_e2ee_handshake(&mut client, "4920").unwrap();
        let mut v_e2ee = crate::shared::crypto::E2eeStream::new(client, v_keys);

        let creds = Credentials::new("", "").with_pin("4920");
        let init = handshake::perform(&mut v_e2ee, &creds).unwrap();
        assert_eq!(init.name, "Encrypted Studio");
        assert_eq!((init.width, init.height), (64, 64));

        remote_screen::negotiate(&mut v_e2ee, &[ENC_PROTOCOL_VERSION]).unwrap();
        assert_eq!(read_u8(&mut v_e2ee).unwrap(), 7);
        let _ = crate::features::file_transfer::read(&mut v_e2ee).unwrap();

        let screen = Mutex::new(Framebuffer::new(64, 64).unwrap());
        let mut decoder = Decoder::default();
        v_e2ee.write_all(&[3, 0, 0, 0, 0, 0, 0, 64, 0, 64]).unwrap();
        v_e2ee.flush().unwrap();
        assert_eq!(read_u8(&mut v_e2ee).unwrap(), 0);
        remote_screen::read_update(&mut v_e2ee, &screen, &mut decoder).unwrap();
        assert_eq!(screen.lock().unwrap().pixels, expected_f1);

        server_handle.join().unwrap();
    }
}
