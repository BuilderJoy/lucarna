# Roadmap de Lucarna

Plan para llegar desde el visor básico actual hasta un cliente que se pueda usar a diario contra UltraVNC Server, incluida la transferencia de archivos.

## Dónde estamos (2026-10-18)

| Área | Estado |
|---|---|
| Handshake RFB 3.3 / 3.8, seguridad None y VNC Auth | ✅ |
| Pantalla: Raw, CopyRect, DesktopSize, ZRLE, Zlib, Hextile, LastRect | ✅ |
| Teclado, ratón, rueda, Cmd→Ctrl en macOS | ✅ |
| Portapapeles de texto Latin-1 en ambos sentidos | ✅ |
| Botón Ctrl+Alt+Del | ✅ |
| Tema, formulario de conexión, spinner hasta el primer frame | ✅ |
| Fase 0: lectura con buffer, timeouts, subida parcial de textura | ✅ |
| Fase 1.1–1.2: `Decoder` con zlib persistente y ZRLE | ✅ |
| Fase 1.3–1.4: Zlib, Hextile y LastRect | ✅ |
| Fase 3.5: dashboard con conexiones guardadas, contraseñas en el llavero del SO | ✅ |
| Fase 2.1–2.2: transferencia de archivos (unidades, navegar, descargar, subir, cola/drag & drop, crear carpeta, renombrar y borrar) | ✅ (pendiente zip de carpetas `!UVNCDIR-` y prueba con UltraVNC en Windows) |
| Fase 3.2, 3.3, 3.4, 3.7: solo ver, escalado Fit/1:1, pantalla completa, menú de teclas | ✅ |
| Fase 3.1: cursor local (pseudo-encoding Cursor) | ✅ |
| Fase 4: MS-Logon II y III (usuario y contraseña de Windows) | ✅ en papel y tests, falta probar con un UltraVNC real |
| Todo lo demás | ❌ (ver fases) |

**Límite importante del diseño actual:** el bucle de `session/mod.rs` corta la conexión si recibe un tipo de mensaje que no conoce, y no hay forma de saltarse un mensaje RFB desconocido porque el protocolo no manda su longitud. **Regla:** no se anuncia una codificación ni se activa una función del servidor hasta que su parser está listo y probado.

---

## Fase 0: arreglos rápidos de rendimiento y robustez ✅ (2026-10-17)

Es lo primero porque explica parte de la lentitud actual y no depende de nada.

| # | Tarea | Dónde | Detalle |
|---|---|---|---|
| 0.1 | Lectura con buffer | `session/mod.rs` | Hoy cada `read_u8`/`read_u16` es una llamada al sistema sobre el `TcpStream` sin buffer. Hay que envolver el lector en `BufReader::with_capacity(64 KiB, …)`. Las escrituras siguen yendo por `Outbound`, que tiene su propio clon del socket. |
| 0.2 | Timeout al conectar | `session/mod.rs` | `TcpStream::connect` se puede colgar más de un minuto con una IP muerta. Hay que resolver con `to_socket_addrs` y usar `connect_timeout(10 s)`. |
| 0.3 | Subir solo la región modificada | `remote_screen/{framebuffer,view}.rs` | Hoy cada update convierte y sube la imagen entera (1920×1080 son ~8 MB por frame). Se cambia `dirty: bool` por un rectángulo acumulado y se usa `TextureHandle::set_partial`. |
| 0.4 | Conversión BGRX→RGBA solo del rectángulo | `remote_screen/view.rs` | Va junto con 0.3. |

**Prueba:** el test `live_server` debe seguir pasando. Para medir, se cronometra el tiempo hasta el primer frame antes y después contra el servidor real.

**Tamaño:** S (1 día).

---

## Fase 1: codificaciones comprimidas (prioridad máxima)

Sin compresión, Lucarna no sirve por internet. UltraVNC Server soporta ZRLE, Tight, Zlib y Hextile, y todas son estándar (RFC 6143 y la documentación de la comunidad RFB). La codificación propietaria "Ultra" no hace falta.

**Dependencias:** `flate2` (backend `miniz_oxide`, puro Rust) y más adelante `zune-jpeg`. **Las dos ya están en `Cargo.lock` como dependencias transitivas de eframe**, así que agregarlas no suma crates nuevos a la compilación.

### 1.1 Estado del decodificador ✅ (2026-10-17)

**Hecho, más simple que lo planeado:** `Decoder` vive en `remote_screen/protocol.rs` con un solo campo `zrle`, y ZRLE está en `remote_screen/zrle.rs`. El árbol `decoder.rs` + `encodings/` de abajo se crea cuando haya más de un encoding comprimido.

ZRLE y Tight usan **un flujo zlib que dura toda la sesión**, no uno por rectángulo. Por eso hace falta estado entre updates:

```
remote_screen/
  decoder.rs        struct Decoder { zrle: Decompress, zlib: Decompress, tight: [Decompress; 4] }
  encodings/
    zrle.rs
    zlib.rs
    hextile.rs
    tight.rs
```

`session::run` crea un `Decoder` y se lo pasa a `read_update(&mut s, &screen, &mut decoder)`. Los encodings nuevos se agregan **a la vez** en la lista de `negotiate` y en el `match` de `read_update`.

### 1.2 ZRLE (encoding 16): primero ✅ (2026-10-17)

Formato (RFC 6143 §7.7.6):

- Por rectángulo: `u32` de longitud seguido de los datos zlib. Se inflan en el flujo persistente.
- La imagen se divide en tiles de 64×64, de izquierda a derecha y de arriba abajo. Los bordes pueden ser más chicos.
- **CPIXEL:** con nuestro formato (32 bpp, depth 24, little-endian, máximos de 255) son **3 bytes**: B, G, R.
- El tipo de cada tile viene en un byte de subencoding:
  - `0`: raw, `w*h` CPIXELs.
  - `1`: color sólido, 1 CPIXEL.
  - `2..=16`: paleta empaquetada. Primero va la paleta de N CPIXELs y después los índices a 1, 2 o 4 bits según N (2, 3–4 o 5–16). Cada fila se rellena hasta el byte.
  - `128`: RLE plano. Cada corrida es un CPIXEL más la longitud, que se calcula como la suma de bytes hasta uno distinto de 255, más 1.
  - `130..=255`: RLE con paleta de `sub-128` colores. Si el índice tiene el bit alto en 1, le sigue una longitud de corrida; si no, es un solo píxel.
  - `17..=127` y `129`: inválidos, se cortan con error.

**Validaciones contra un servidor malicioso:**

- Tope de longitud comprimida, por ejemplo 64 MiB.
- Inflar con salida acotada.
- Ninguna corrida puede pasarse del tile.
- Ningún índice puede salirse de la paleta.
- Sobran o faltan bytes al terminar el rectángulo → error.

**Tests unitarios:** tiles fabricados a mano para cada subencoding, más casos inválidos (corrida que desborda, índice fuera de paleta, subencoding 17).

**Test en vivo:** x11vnc del contenedor Docker ya habla ZRLE, así que no hace falta Windows.

**Tamaño:** M (2–3 días).

### 1.3 Zlib (6) y Hextile (5): baratas, para servidores viejos ✅ (2026-10-17)

- **Zlib:** píxeles raw dentro de un flujo zlib persistente. Son ~30 líneas una vez que existe `Decoder`.
- **Hextile:** tiles de 16×16 sin zlib, con bits de flags `Raw`, `BackgroundSpecified`, `ForegroundSpecified`, `AnySubrects` y `SubrectsColoured`. El fondo y el frente se arrastran de un tile al siguiente.

**Tamaño:** S (1 día las dos).

### 1.4 Pseudo-encodings útiles

**LastRect ✅ (2026-10-17).** CompressLevel no se anuncia todavía: el servidor elige un nivel razonable por defecto, y se agrega si hace falta ajustar ancho de banda contra CPU. QualityLevel va con Tight.

| Pseudo | Valor | Para qué |
|---|---|---|
| LastRect | -224 | El servidor puede mandar el número de rectángulos como 0xFFFF y cerrar con este. Tight lo usa. |
| CompressLevel | -256 … -247 | Nivel de compresión 0–9. |
| QualityLevel | -32 … -23 | Calidad JPEG en Tight. Va en la Fase 1.5. |

### 1.5 Tight (7): después de la transferencia de archivos

Es la mejor opción con internet lento porque usa JPEG en zonas con foto, pero es la más compleja:

- 4 flujos zlib con un byte de control que puede reiniciarlos.
- Modos fill, JPEG y basic.
- Filtros copy, palette y gradient.
- TPIXEL de 3 bytes.
- Longitud compacta de 1 a 3 bytes.

**Tamaño:** M-L (3–4 días). Se deja para después porque ZRLE ya da casi toda la ganancia en escritorios Windows típicos.

---

## Fase 2: transferencia de archivos de UltraVNC (la razón de existir)

Es lo que ningún cliente de macOS ofrece hoy. **El protocolo no tiene especificación pública**: la fuente de verdad es el código de UltraVNC (`rfb/rfbproto.h`, el visor en `vncviewer/FileTransfer.cpp` y el servidor en `winvnc/vncclient.cpp`).

### 2.1 Spike de investigación (antes de escribir código) ✅ (2026-10-18)

**Hecho:** [docs/ultravnc-ft.md](ultravnc-ft.md), leído del código fuente oficial (commit `0cd8b41`) y de `libvncserver/rfbserver.c`. Documenta tanto lectura (permiso, unidades, listados, descarga) como escritura (`FileTransferOffer` con los 4 bytes extra `sizeH` fuera de `length`, `FileAcceptHeader`, tamaño de bloque negociado por `FileTransferProtocolVersion` vs 8 KiB fijo en `libvncserver`, comandos `CDirCreate`/`CFileDelete`/`CFileRename` y sus respuestas `CommandReturn`).

1. Leer `rfbproto.h` de la versión estable actual y anotar en `docs/ultravnc-ft.md`:
   - el tipo de mensaje (`rfbFileTransfer`, que se espera sea 7),
   - la estructura `rfbFileTransferMsg`, que se espera sea `type, contentType, contentParam, pad, u32 size, u32 length` seguida de los datos,
   - cada `contentType`: pedido de directorio, paquete de directorio, pedido de archivo, cabecera, paquete, fin, abortar, oferta, aceptación, comando, respuesta de comando y acceso.
2. Averiguar **cómo se activa**: si el cliente debe anunciar un pseudo-encoding o versión de protocolo de FT, si el servidor pide permiso (`FileTransferAccess`) y qué hace cuando `FileTransferEnabled=0` en `ultravnc.ini`.
3. Averiguar el formato de los listados: struct de Windows `WIN32_FIND_DATA` con FILETIME, nombres en codepage o UTF-8 según versión, y la marca de unidades `C:` y `D:`.
4. Averiguar la compresión por paquete (zlib opcional según `contentParam`) y el tamaño de bloque (~8 KiB).
5. **Capturar tráfico real** con Wireshark entre el visor oficial de UltraVNC y el servidor de prueba. Se guarda un `.pcap` de muestra para tests de regresión, con contraseñas de prueba.
6. Comparar al menos dos versiones del servidor, por ejemplo la 1.4.x y la última estable, y anotar las diferencias.

**Salida:** documento con los bytes exactos de cada mensaje, confirmados contra la captura. **Tamaño:** M (2–3 días).

### 2.2 Implementación (nuevo slice) ✅ (2026-10-18)

**Hecho:** `features/file_transfer/` con `protocol.rs`, `transfer.rs` y `view.rs`. Ventana de dos columnas (`Local` | `Remote`, sin añadir `rfd`):

- **Lectura:** permiso, unidades, navegación con doble clic en ambos paneles y descarga a la carpeta local activa (por defecto `~/Downloads`), escribiendo a `.lucarna-part` y renombrando al terminar, con nombres saneados.
- **Escritura:** subida de archivos (`FileTransferOffer` → `FileAcceptHeader` → `FilePacket` en hilo propio con compresión zlib solo cuando reduce el bloque → `EndOfFile`), tamaño de bloque de 8 KiB por defecto compatible con `libvncserver` y 32 KiB cuando el servidor anuncia `FileTransferProtocolVersion` v3+, cola de subida, arrastrar y soltar desde el sistema (`egui::DroppedFile`), cancelación de descargas y subidas, y comandos remotos (`+ Folder`, `Rename`, `Delete` con confirmación). Mientras un campo de texto de la UI tiene el foco, no se reenvían teclas al servidor remoto.
- **Pendiente para más adelante:** transferencia de carpetas completas como zip (`!UVNCDIR-`, que requiere `zip`) y prueba contra UltraVNC Server en Windows.

```
features/file_transfer/
  mod.rs
  protocol.rs   mensajes FT: encode/decode, validados
  transfer.rs   máquina de estados de upload/download, colas, cancelación
  view.rs       panel de dos columnas local | remoto
```

- **Session:** se agrega el brazo `7 => file_transfer::read_message(&mut s)?` y se manda un `Event::FileTransfer(msg)` a la UI.
- **Archivos grandes:** la escritura a disco va en el hilo de sesión o en uno propio, nunca en el de la UI. Hay que streamear, nunca cargar el archivo completo en memoria.
- **Funciones, en orden:**
  1. Listar unidades y navegar directorios remotos.
  2. Descargar archivo.
  3. Subir archivo.
  4. Barra de progreso, cancelar y cola.
  5. Crear carpeta, renombrar y borrar (comandos FT).
  6. Arrastrar y soltar desde el Finder (`egui` `DroppedFile`).
  7. Carpetas completas: UltraVNC las manda comprimidas como zip, así que hay que confirmarlo en el spike.
- **Seguridad:** un servidor malicioso puede mandar nombres con `..`, rutas absolutas o caracteres prohibidos. Hay que sanear todo nombre antes de tocar el disco local y no escribir nunca fuera de la carpeta destino elegida. Los tamaños y longitudes se acotan como en el resto del código.
- **UI:**
  - Botón "Archivos" en la barra superior que abre una ventana o un panel lateral.
  - Dos columnas, con navegación con doble clic, tamaño, fecha y barra de ruta.
  - Diálogo nativo para elegir la carpeta local: `rfd` sería una dependencia nueva. Se evalúa contra un navegador local hecho en egui y se elige lo más chico.

**Tests:**

- Unitarios de codificación y decodificación contra bytes sacados del `.pcap`.
- Saneamiento de nombres con `..`, `C:\`, `/etc/passwd` y nombres vacíos.
- Test `#[ignore]` en vivo contra una VM Windows: subir un archivo, bajarlo y comparar hashes.

**Tamaño:** L (1–2 semanas).

**Hito v0.1, primer release público:** Fase 0 + ZRLE + transferencia de archivos + empaquetado (Fase 6).

---

## Fase 3: ergonomía de uso diario

| # | Tarea | Detalle | Tamaño |
|---|---|---|---|
| 3.1 | **Cursor local ✅ (2026-10-17)** | Se anuncia el pseudo-encoding Cursor (-239). El rect trae `x,y` = hotspot, `w,h`, `w*h` píxeles y una máscara de `ceil(w/8)*h` bytes. Se dibuja como textura que sigue al ratón local y se oculta el cursor del SO sobre el viewport (`CursorIcon::None`). Se valida el tamaño, por ejemplo hasta 256×256. Elimina la sensación de retraso. | S |
| 3.2 | Modo solo ver ✅ (2026-10-17) | Botón "View only" en la barra que salta `input.forward` y `forward_paste`. | XS |
| 3.3 | Escalado ✅ (2026-10-17) | Botón Fit / 1:1 en la barra. 1:1 es un píxel remoto por píxel físico (en Retina, 0.5 puntos) con `ScrollArea`; **solo las barras de desplazamiento mueven la vista**, porque la rueda y el arrastre son del equipo remoto. `Viewport` ahora también conoce la parte visible. | S |
| 3.4 | Pantalla completa ✅ (2026-10-17) | Botón "Full screen" y atajo ⌘⇧F (Ctrl+Shift+F fuera de macOS), que se consume antes de reenviar teclas. En pantalla completa la barra flota sobre la imagen (no la desplaza) y aparece al acercar el ratón al borde superior o mientras haya un menú abierto. | S |
| 3.5 | Conexiones guardadas ✅ (2026-10-17) | Lista de servidores recientes con nombre y dirección. Se activa la feature `persistence` de eframe, que no agrega crates nuevos. **Hecho:** el dashboard de inicio lista las conexiones y permite conectar, editar y borrar. Nombre y dirección van a `connections.txt` en la carpeta de datos de eframe (feature `persistence`). Las contraseñas van solo al llavero del SO con el crate `keyring` 4 (Keychain, Credential Manager o Secret Service), una entrada por dirección. | S-M |
| 3.6 | Reconexión | Botón "Reconectar" en el formulario cuando la sesión se cae, con la última dirección precargada. | XS |
| 3.7 | Teclas especiales ✅ parcial (2026-10-17) | Menú "Keys" con Ctrl+Esc, Alt+Tab, Alt+F4, tecla Windows (0xffeb) y Print Screen; se pulsan en orden y se sueltan al revés. **Falta AltGr** para teclados en español: hoy los caracteres se mandan como keysym ya resuelto, y hay que probar si UltraVNC los acepta bien (`@`, `\`, `{`). | S |
| 3.8 | Portapapeles extendido | Pseudo-encoding Extended Clipboard (0xC0A1E5CE) para UTF-8. Hay que verificar en el spike si UltraVNC lo soporta. Si no, se mantiene Latin-1. | M |
| 3.9 | Campana | Mensaje 2: sonido o rebote del ícono del dock. | XS |

---

## Fase 4: MS-Logon (usuario y contraseña de Windows) ✅ (2026-10-17)

**Hecho:** [docs/ultravnc-auth.md](ultravnc-auth.md) y `connection/mslogon.rs`. Lo que el spike descubrió y cambió el plan:

- **Hay dos versiones:** II (0x71, DH de 31 bits) y **III (0x76, X25519)** en el UltraVNC reciente. El servidor anuncia siempre las dos. Lucarna implementa ambas y prefiere la III.
- **El III no usa AES-GCM** aunque sus comentarios lo digan: la clave de 8 bytes (SHA-256 del secreto, recortada) cifra con DES-CBC, igual que el II. Mejora el intercambio de claves, pero el cifrado de las credenciales sigue siendo DES. Detalle y recomendación de túnel en el documento.
- **Dependencias nuevas:** `x25519-dalek` (sin features por defecto; única crate nueva en compilarse), y `getrandom` y `sha2`, que ya estaban en el árbol por eframe.
- **UI:** campo "User" siempre visible en el dashboard, guardado en `connections.txt` (tercera columna opcional, los archivos viejos siguen cargando). La contraseña va al llavero como antes. Sin usuario solo se usa VNC auth/None; si el servidor solo ofrece MS-Logon, el error pide un usuario.
- **Verificación:** el cifrado coincide con los vectores del `d3des.c` original compilado en el Mac. El flujo completo se probó contra un servidor simulado en los tests. **Falta probar contra un UltraVNC Server real**, que no se puede levantar en Linux ni Docker.
- **No soportado:** SC Prompt (0x68, el usuario remoto acepta la conexión) y las capas extra de autenticación (`0xFFFFFFFF`): dan un error claro.

Plan original, para referencia:

Muchos UltraVNC en empresas usan MS-Logon II en vez de contraseña VNC.

- **Spike:** confirmar en `rfbproto.h` y en `vncviewer/ClientConnection.cpp`:
  - el tipo de seguridad (se espera **113** para MS-Logon II),
  - el intercambio Diffie-Hellman (el servidor manda generador, módulo y su clave pública como `u64`),
  - el cifrado de usuario (256 bytes) y contraseña (64 bytes) con la clave derivada.

  El crate `des` ya está en el proyecto.
- **Código:** `connection/ms_logon.rs`. En `handshake.rs` se agrega 113 a la lista de tipos aceptados, con prioridad sobre 2 cuando el usuario llenó el campo de usuario.
- **UI:** campo "Usuario" opcional en el formulario. Se muestra siempre, o solo si el servidor ofrece 113. Lo segundo exige un reintento, así que conviene empezar mostrándolo siempre.
- **Test:** VM Windows con MS-Logon activado.
- **Riesgo:** DH con `u64` es criptografía débil por diseño de UltraVNC. Se documenta y se recomienda usar túnel (ver Fase 7).

**Tamaño:** M (2–3 días).

---

## Fase 5: chat y repetidor

### 5.1 Chat (mensaje TextChat, 11)

- **Formato esperado:** `type, 3 pad, u32 length`. Hay longitudes especiales para abrir (0xFFFFFFFF), cerrar (0xFFFFFFFE) y terminar (0xFFFFFFFD). Se confirma en el spike de la Fase 2, que lee el mismo archivo.
- **Slice:** `features/chat/` con una ventana flotante de mensajes y la notificación al recibir uno.
- **Tamaño:** S-M.

### 5.2 Repetidor UltraVNC

Sirve para conectar a máquinas detrás de NAT sin abrir puertos.

- **Modo I:** el cliente se conecta al repetidor y manda `host:port` del destino, rellenado a 250 bytes.
- **Modo II:** el cliente manda `ID:1234`, rellenado a 250 bytes.
- **Confirmar:** si el repetidor manda primero un `RFB 000.000\n` y cómo responde el cliente.
- **Código:** `connection/repeater.rs`, que corre antes de `handshake::perform`. En el formulario se agrega un campo "Repetidor" opcional.
- **Tamaño:** S-M.

---

## Fase 6: empaquetado y distribución

| Plataforma | Formato | Notas |
|---|---|---|
| macOS | `.app` + `.dmg` | Ícono `.icns` generado desde `assets/logo.svg`. Firma y notarización requieren cuenta de Apple Developer (99 USD/año); sin ellas, Gatekeeper bloquea la app. Binario universal arm64 + x86_64 con `lipo`. |
| Windows | `.exe` + MSI | Ícono `.ico` incrustado. `windows_subsystem = "windows"` para que no abra consola. Firma opcional; sin ella, SmartScreen avisa. |
| Linux | AppImage o tarball | Ícono `.png` + `.desktop`. |

- **Ícono en la ventana ✅ (2026-10-17):** `ViewportBuilder::with_icon` con `assets/logo.png` incrustado. El PNG se genera desde el SVG con `rsvg-convert` (comando en el README).
- **CI:** GitHub Actions con matriz macOS/Windows/Linux que corre `cargo fmt --check`, `cargo clippy -D warnings` y `cargo test`, y un job con el contenedor x11vnc para los tests `--ignored`. Los releases se publican al crear un tag.

**Tamaño:** M (2–3 días, más el trámite de Apple).

---

## Fase 7: cifrado

- **Plugins DSM de UltraVNC (SecureVNC, MSRC4):**
  - Hay que investigar si su fuente está publicada y bajo qué licencia, y si el formato es estable entre versiones.
  - Si alguno es cerrado o no documentado, **no se replica**.
  - **Tamaño:** L con riesgo alto. Se hace solo si la comunidad lo pide.
- **Alternativa recomendada, que no requiere código:** tunelar la conexión.
  - Cloudflare Tunnel con Access (`cloudflared access tcp`).
  - Túnel SSH, o una VPN como WireGuard o Tailscale.

  Conviene documentarlo en el README como forma oficial de usar Lucarna por internet, porque es más seguro que cualquier DSM o que VNC Auth/MS-Logon solos.
- **Opcional:** VeNCrypt (tipo 19, TLS) para servidores que no son UltraVNC, como TigerVNC y x11vnc. Requiere `rustls`, una dependencia nueva y pesada, así que se evalúa entonces.

---

## Fuera de alcance por ahora

- Codificación "Ultra" y "Ultra2" (propietarias, con compresión LZO); ZRLE y Tight las reemplazan.
- ZYWRLE (con pérdida, poco usada).
- Selección de monitor en servidores multipantalla (UltraVNC tiene mensajes propios); se reevalúa después de la v0.1.
- Grabación de sesión, audio e impresión remota.

## Infraestructura de pruebas necesaria

| Para | Qué hace falta |
|---|---|
| Fases 0, 1 y 3 | Contenedor x11vnc actual en `tests/server/` (ya existe). |
| Fases 2, 4 y 5 | **VM Windows** (UTM en Apple Silicon o Parallels) con UltraVNC Server en 2–3 versiones, o acceso al servidor real en ventanas de mantenimiento. |
| Regresión de FT | `.pcap` capturados con el visor oficial, guardados en `tests/captures/`. |

## Orden y estimación global

| Orden | Fase | Tamaño estimado |
|---|---|---|
| 1 | Fase 0: rendimiento y robustez ✅ | 1 día |
| 2 | Fase 1.2–1.4: ZRLE, Zlib, Hextile ✅ | 3–4 días |
| 3 | Fase 2: transferencia de archivos | 2–3 semanas con el spike |
| 4 | Fase 6: empaquetado → **release v0.1** | 2–3 días |
| 5 | Fase 3: ergonomía | 1 semana |
| 6 | Fase 4: MS-Logon | 2–3 días |
| 7 | Fase 1.5: Tight | 3–4 días |
| 8 | Fase 5: chat y repetidor | 3–4 días |
| 9 | Fase 7: cifrado | Según demanda |

Las estimaciones son orientativas, para una persona a ritmo sostenido. El mayor riesgo de calendario es la Fase 2, por el protocolo sin documentar y las diferencias entre versiones.
