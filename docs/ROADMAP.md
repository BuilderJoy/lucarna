# Roadmap de Lucarna

Plan para llegar desde el visor básico actual hasta un cliente que se pueda usar a diario contra UltraVNC Server, incluida la transferencia de archivos.

## Dónde estamos (2026-10-18)

| Área | Estado |
|---|---|
| Handshake RFB 3.3 / 3.8, seguridad None y VNC Auth | ✅ |
| Pantalla: Tight (con JPEG), ZRLE, Zlib, Hextile, Raw, CopyRect, DesktopSize, LastRect | ✅ |
| Teclado, ratón, rueda, Cmd→Ctrl en macOS | ✅ |
| Portapapeles de texto UTF-8 (`Extended Clipboard` `0xC0A1E5CE`) con fallback automático a Latin-1 | ✅ |
| Botón Ctrl+Alt+Del | ✅ |
| Tema, formulario de conexión, spinner hasta el primer frame | ✅ |
| Fase 0: lectura con buffer, timeouts, subida parcial de textura | ✅ |
| Fase 1.1–1.2: `Decoder` con zlib persistente y ZRLE | ✅ |
| Fase 1.3–1.4: Zlib, Hextile y LastRect | ✅ |
| Fase 3.5: Address Book con vista previa del escritorio, etiquetas/grupos, favoritos, opciones por conexión y contraseñas en el llavero | ✅ |
| Fase 2.1–2.2: transferencia de archivos (unidades, navegar, descargar, subir, cola/drag & drop, crear carpeta, renombrar y borrar) | ✅ (carpetas: se descargan como `.zip` sin descomprimir; pendiente prueba con UltraVNC en Windows) |
| Fase 3.1–3.9: cursor local, solo ver, escalado Fit/1:1, pantalla completa, reconexión rápida, menú de teclas con AltGr/símbolos ES, Extended Clipboard UTF-8 y campana (Bell) | ✅ |
| Fase 4: MS-Logon II y III (usuario y contraseña de Windows) | ✅ en papel y tests, falta probar con un UltraVNC real |
| Fase 5: chat de texto y repetidor UltraVNC (Modo I y II) | ✅ repetidor probado de punta a punta; chat por tests y formato del código oficial |
| Fase 7: túnel documentado como forma oficial de uso por internet (sin VeNCrypt ni DSM) | ✅ |
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

### 1.5 Tight (7) ✅ (2026-10-18)

**Hecho:** `remote_screen/tight.rs`, siguiendo `libvncclient/tight.c`: fill, JPEG (con `zune-jpeg`, que ya se compilaba por eframe, con ancho y alto máximos antes de decodificar) y básico con filtros copy, palette y gradient sobre 4 flujos zlib persistentes que el byte de control puede reiniciar. TPIXEL en orden R, G, B (confirmado también en `Pack24` del codificador de UltraVNC). Validaciones que libvncclient no hace: índice de paleta fuera de rango y JPEG de otro tamaño que el rectángulo.

- **Negociación:** Tight va **primero** en la lista, con QualityLevel 8 (`-24`): los servidores solo usan JPEG en zonas tipo foto y el texto sigue sin pérdida. Calidad fija; se vuelve opción por conexión si alguien la necesita.
- **Prueba en vivo (`live_tight_matches_raw`):** la misma pantalla de x11vnc con Raw y con Tight sin JPEG sale **idéntica byte a byte**; con JPEG, la desviación media es menor de 3 por canal. Se ejercitaron fill, JPEG, copy y palette con los streams 0, 1 y 2. La imagen de pruebas ahora pinta rampas de 256 colores y truecolor (`tests/server/colors.sh`).
- **El gradiente** no lo usan ni libvncserver ni el codificador de UltraVNC (solo servidores TightVNC): está cubierto por test unitario.
- **Fuera:** la variante "NoZlib/PNG" (`0x0A`) de TigerVNC/TurboVNC, que solo se manda si el cliente la anuncia.

Plan original, para referencia:

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
- **Carpetas (2026-10-18):** se descargan como `nombre.zip` (el zip `!UVNCDIR-` que arma el servidor), sin descomprimir en la app; subir carpetas sigue siendo crear la carpeta y subir sus archivos. **Pendiente:** prueba contra UltraVNC Server en Windows.

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
| 3.5 | Conexiones guardadas y Address Book ✅ (2026-10-18) | Rediseñado al estilo RealVNC Viewer: barra superior de búsqueda y conexión rápida, barra lateral con `All connections`, `Favorites`, `Recent` y etiquetas (`LABELS`), vista en cuadrícula o lista con miniatura del escritorio remoto (`previews/*.lcpv` comprimida con zlib, o ilustración vectorial si aún no se conectó), menú contextual completo (`Connect`, `View only`, `Favorites`, `Labels`, `Rename` F2, `Duplicate` ⌘D, `Clear preview`, `Delete`, `Properties…` Alt+Enter), importar/exportar y opciones por conexión (`Fit`/`1:1`, `View only`, `Full screen`, `Share clipboard`, `Save preview`). Contraseñas solo en el llavero del SO (`keyring`). | M |
| 3.6 | Reconexión ✅ (2026-10-18) | Cuando una sesión termina inesperadamente o falla al conectar, la barra inferior muestra el motivo junto con un botón **Reconnect (`<servidor>`)** que relanza el último `ConnectRequest` con un clic. | XS |
| 3.7 | Teclas especiales y AltGr ✅ (2026-10-18) | Menú "Keys" con Ctrl+Esc, Alt+Tab, Alt+F4, tecla Windows (`0xffeb`), Print Screen, `AltGr (ISO Level 3)` (`0xfe03`) y botonera rápida de símbolos españoles/AltGr (`@`, `\`, `|`, `{`, `}`, `[`, `]`, `~`, `#`, `€`). Además, al escribir con `Option` en macOS se suelta temporalmente `Alt` remoto alrededor del carácter para que servidores Windows sinteticen su propio `AltGr` sin interpretarlo como `Alt+tecla`. | S |
| 3.8 | Portapapeles extendido ✅ (2026-10-18) | Pseudo-encoding `Extended Clipboard` (`0xC0A1E5CE` / `-1063131698`) anunciado en `SetEncodings`. Si el servidor envía `CLIP_CAPS`, `RemoteClipboard` responde con capacidades UTF-8 y negocia `CLIP_PROVIDE`, `CLIP_NOTIFY`, `CLIP_REQUEST` y `CLIP_PEEK` con compresión zlib y normalización CRLF/NUL; si el servidor no lo anuncia, mantiene Latin-1 automáticamente. | M |
| 3.9 | Campana ✅ (2026-10-18) | Mensaje 2 (`Bell`): `session::run` emite `Event::Bell`, solicita atención de la ventana (`ViewportCommand::RequestUserAttention`) y muestra un indicador visual `Bell` en la barra de sesión durante 2 s (revelando la barra flotante si está en pantalla completa). | XS |

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

## Fase 5: chat y repetidor ✅ (2026-10-18)

**Hecho, con lo mínimo** (leído de `TextChat.cpp`, `ClientConnection::NegotiateProxy` y el código del repetidor de UltraVNC):

- **Chat:** `features/chat/` (`protocol.rs`, `view.rs`). Mensaje 11 con `3 pad + u32 length`; la longitud `0xFFFFFFFF/FE/FD` es Open/Close/Finished y cualquier otra es texto (máx. 16384 bytes, Latin-1 como el portapapeles). Botón **Chat** en la barra: abrirlo o cerrarlo se lo pide también al servidor. Si el servidor abre el chat o escribe, la ventana se abre sola y el Dock avisa. Mientras se escribe en ella no se reenvían teclas ni ratón al equipo remoto. Verificado con tests de formato y de límites; **sin probar contra un servidor**: x11vnc solo ofrece chat con el programa auxiliar `ssvnc`, que no está en el contenedor.
- **Repetidor:** `handshake::via_repeater`. Se conecta al repetidor, que saluda con `RFB 000.000\n`; se le manda el destino en 250 bytes rellenos con ceros, `host:puerto` (Modo I) o `ID:1234` (Modo II), y después el repetidor reenvía el saludo real del servidor. Campo **Repeater** en las propiedades de la conexión, guardado como séptima columna de `connections.txt`; el campo **VNC Server** pasa a ser el destino. Probado de punta a punta contra x11vnc con un relé mínimo escrito en el test según el código del repetidor (`live_repeater`).
- **Lo que quedó fuera a propósito:** chat Unicode (pseudo-encoding `0xFFFF8006`, hoy se manda Latin-1), la rama "el servidor no es un repetidor" (si pones un repetidor y no saluda con 0.0, falla con un error claro en vez de adivinar) y un puerto por defecto para el repetidor (el de UltraVNC suele ser 5901: escríbelo).
- **Aviso:** en Modo II el repetidor retiene la conexión hasta que el servidor con ese ID se conecte a él; si tarda más de 10 s, Lucarna da un timeout.

Plan original, para referencia:

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

## Fase 6: empaquetado y distribución ✅ (2026-10-18)

**Hecho:** `.github/workflows/packages.yml` construye los instaladores en cada PR y, con un tag `v*`, los publica como release (el último, *Latest*) en GitHub. La página del release empieza con `packaging/release-notes.md`: enlaces de descarga e instrucciones de instalación por sistema.

- **macOS:** `.dmg` con `Lucarna.app` universal (arm64 + x86_64 con `lipo`), ícono `.icns` y firma ad-hoc. **Aclaración importante:** como todavía no se cuenta con la aprobación y notarización de Apple Developer, en macOS Gatekeeper bloquea la instalación directa del `.dmg` descargado, por lo que **actualmente en Mac se debe ejecutar o instalar utilizando Rust** (`cargo run --release` o generando el bundle local con `./packaging/macos/bundle.sh`). Probado en local: la app arranca desde el bundle compilado en la máquina.
- **Windows:** instalador de Inno Setup (`.exe`), en español e inglés, instalable sin permisos de administrador, sin consola (`windows_subsystem`). Sin firma de código: SmartScreen avisa.
- **Linux:** `.deb` (cargo-deb, con `.desktop` e ícono) y `.tar.gz` portable, compilados en Ubuntu 22.04 para que funcionen en glibc más viejas.
- **Versión:** `0.1.0` (SemVer 0.x: aún en desarrollo, puede cambiar sin aviso). Se publicó primero como *pre-release*, pero GitHub no la mostraba en la portada del repo, así que se pasó a release normal; el `0.x` ya avisa del estado. El 1.0.0 queda para cuando esté probado contra UltraVNC real y estable.
- **Íconos:** `packaging/icons.sh` regenera `assets/lucarna.icns` y `assets/lucarna.ico` desde el SVG. Se añadió `LICENSE` (texto de la GPL-2.0), que va dentro de cada paquete.
- **Fuera por ahora:** notarización de Apple y firma de código de Windows (cuestan dinero y cuentas), AppImage/Flatpak, ARM en Windows y Linux, y el ícono dentro del `.exe` (el acceso directo usa el `.ico`).

Plan original, para referencia:

| Plataforma | Formato | Notas |
|---|---|---|
| macOS | `.app` + `.dmg` | Ícono `.icns` generado desde `assets/logo.svg`. Firma y notarización requieren cuenta de Apple Developer (99 USD/año); sin ellas, Gatekeeper bloquea la app. Binario universal arm64 + x86_64 con `lipo`. |
| Windows | `.exe` + MSI | Ícono `.ico` incrustado. `windows_subsystem = "windows"` para que no abra consola. Firma opcional; sin ella, SmartScreen avisa. |
| Linux | AppImage o tarball | Ícono `.png` + `.desktop`. |

- **Ícono en la ventana ✅ (2026-10-17):** `ViewportBuilder::with_icon` con `assets/logo.png` incrustado. El PNG se genera desde el SVG con `rsvg-convert` (comando en el README).
- **CI:** GitHub Actions con matriz macOS/Windows/Linux que corre `cargo fmt --check`, `cargo clippy -D warnings` y `cargo test`, y un job con el contenedor x11vnc para los tests `--ignored`. Los releases se publican al crear un tag.

**Tamaño:** M (2–3 días, más el trámite de Apple).

---

## Fase 7: cifrado ✅ la parte sin código (2026-10-18)

**Hecho:** el README tiene la sección "Using Lucarna over the internet" con túnel SSH, VPN (Tailscale/WireGuard) y Cloudflare Tunnel como forma oficial de uso, y explica qué expone cada login (VNC auth, MS-Logon II y III) y que la sesión viaja sin cifrar. **No se implementaron** VeNCrypt ni los plugins DSM: el túnel cubre ambos casos y queda pendiente hasta que alguien los necesite de verdad.

Plan original, para referencia:

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

## Fase 8: SSH integrado (en curso)

Opción elegida en lugar de un servidor propio estilo RealVNC (servidor, protocolo cifrado y nube para atravesar NAT): SSH da el mismo cifrado y la transferencia de archivos sin construir ni mantener nada de eso. Se usa el **cliente `ssh` del sistema**, sin crates nuevos.

### 8.1 Túnel SSH ✅ (2026-10-19)

- **Campo "SSH" en las propiedades de la conexión** (`user@host[:port]`) y "SSH password" opcional, en el llavero bajo la cuenta `ssh:…`. Se guarda como 8.ª columna de `connections.txt`; los archivos viejos siguen cargando.
- **`features/ssh`:** lanza `ssh -N -L` con opciones fijas y conecta VNC por el túnel. Si el VNC está en el propio host SSH, el túnel termina en su loopback, así que UltraVNC puede aceptar solo loopback.
- **Falla cerrado:** un destino SSH inválido, o SSH junto con un repetidor, es un error, nunca una conexión sin cifrar. El destino se valida contra la inyección de opciones (`-oProxyCommand=…`) y el host siempre va después de `--`.
- **Contraseña:** modo askpass del propio ejecutable; sin contraseña guardada, `BatchMode=yes` (solo llaves y agente).
- **Errores claros:** usuario o contraseña rechazados, clave del host cambiada (`ssh-keygen -R`), servidor que prohíbe túneles (`AllowTcpForwarding`), VNC inalcanzable, `ssh` no instalado, OpenSSH anterior a 8.4 con contraseña.
- **Probado en vivo** contra un sshd con x11vnc cuyo puerto 5900 no está expuesto (`live_ssh_tunnel`): la imagen solo puede llegar por el túnel. También se provocaron y comprobaron los errores de túnel prohibido y de usuario rechazado.
- **Hallazgo:** el sshd de Alpine trae `AllowTcpForwarding no`; ahora el error lo dice.
- **Pendiente (manual):**
  - Contraseña SSH guardada de punta a punta en macOS: no se puede automatizar, porque leer del llavero una entrada creada por otro programa abre un diálogo de macOS. En uso normal la guarda y la lee el mismo ejecutable.
  - Contraseña desde un cliente **Windows 10/11**: su OpenSSH puede ser anterior a 8.4.

### 8.2 Archivos por SFTP (siguiente)
Códec SFTP v3 sobre `ssh -s … sftp`, integrado en el panel de archivos actual (`FilesView`), reutilizando la misma interfaz de dos columnas local | remoto para cualquier servidor con SSH (incluso cuando el servidor VNC no es UltraVNC, como `x11vnc`, `TigerVNC` o macOS Screen Sharing).

![Tubería Gráfica de 60 FPS y Motor Dual de Archivos](diagrams/06-pipeline-pantalla-archivos.png)

### 8.3 Pruebas en vivo y guía de Windows (siguiente)
Contenedor de pruebas con sshd, prueba `live_ssh` completa y guía para activar OpenSSH Server en Windows y dejar UltraVNC escuchando solo en loopback (`127.0.0.1`).

---

## Fase 9: seguridad y endurecimiento transversal (prioridad máxima)

La seguridad no es una función aislada sino una invariante de todo lo que se agregue de aquí en adelante. En un cliente de control remoto existen **dos amenazas reales**: un atacante en la red (MITM / sniffing) y **un servidor remoto hostil o comprometido** que intente atacar el equipo del técnico a través del visor.

![Arquitectura de Seguridad y Defensa en Profundidad](diagrams/03-seguridad-defensa.png)

```mermaid
flowchart LR
    subgraph B1["Barrera 1: Secretos Locales e Higiene en RAM"]
        KC["Llavero del SO<br/>(Keychain / Cred Manager)<br/>Cero secretos en disco"]
        AP["Modo SSH_ASKPASS<br/>Sin secretos en argv/env<br/>+ Zeroize en RAM"]
        CAT["Catálogo Seguro<br/>connections.txt sin<br/>ProxyCommand ni scripts"]
    end

    subgraph B2["Barrera 2: Transporte Cifrado y Fail-Closed"]
        FC["Política Fail-Closed<br/>Nunca cae a texto plano<br/>Modo «Exigir Túnel SSH»"]
        HK["Host Key Pinning<br/>Huella SHA-256 en UI<br/>Sin ForwardAgent ni X11"]
        CB["Portapapeles Direccional<br/>Off / Solo Enviar /<br/>Solo Recibir / Ambos"]
    end

    subgraph B3["Barrera 3: Escudo Contra Servidor Hostil"]
        VAL["Validador de Rectángulos<br/>MAX_DIM, Framebuffer::contains<br/>Topes Zlib / JPEG anti-OOM"]
        SBX["Sandbox de Archivos (FT/SFTP)<br/>Rechazo de '..', UNC, symlinks<br/>Escritura vía .lucarna-part"]
        CUT["Corte Inmediato<br/>Bytes sobrantes o tipo<br/>desconocido cierran sesión"]
    end

    B1 --> B2 --> B3
```

| # | Tarea | Detalle | Tamaño |
|---|---|---|---|
| 9.1 | **Indicador visual de seguridad y modo «Solo cifrado»** | Candado en la barra de sesión y en las tarjetas del Address Book: `Cifrado (SSH)` vs `Sin cifrar (texto plano)`. Opción por conexión y global para **prohibir conexiones sin túnel SSH** fuera de `127.0.0.1`, evitando conectar por accidente en claro a través de internet o redes Wi-Fi públicas. | S |
| 9.2 | **Verificación y huella de clave SSH (Host Key Pinning)** | Mostrar la huella SHA-256 del servidor SSH al confiar por primera vez y guardar o verificar contra `known_hosts`. Si la clave del servidor cambia, bloquear la conexión de forma cerrada con alerta clara de posible ataque MITM. | S |
| 9.3 | **Dirección del portapapeles (`Off` / `Solo enviar` / `Solo recibir` / `Bidireccional`)** | Un servidor comprometido o compartido puede leer automáticamente lo que el técnico copia en su equipo local (p. ej. contraseñas de su gestor). Reemplazar el booleano `Share clipboard` por control direccional: por defecto permitir **solo local → remoto** al pegar explícitamente, o desactivarlo del todo. | XS |
| 9.4 | **Borrado seguro de secretos en memoria (`Zeroize`)** | Sobrescribir con ceros las contraseñas VNC y SSH en memoria (`Credentials`) apenas termina el handshake o el modo `askpass`, para que no sobrevivan en el heap ni en volcados de memoria (crash dumps). | XS |
| 9.5 | **Aislamiento de SFTP y archivos frente a servidores maliciosos** | Mismas garantías que en UltraVNC FT: rechazar nombres con `..`, separadores de ruta, nombres reservados de Windows (`CON`, `NUL`, `COM1`…), rutas UNC y enlaces simbólicos que apunten fuera de la carpeta de descarga; límites estrictos de tamaño de paquete y escritura atómica vía `.lucarna-part`. | S |
| 9.6 | **Importación segura de catálogos (`connections.txt`)** | Validar que al importar una libreta de direcciones compartida nunca se puedan inyectar opciones de `ssh` (`-oProxyCommand=…`), rutas de ejecutables ni scripts locales. | XS |

---

## Fase 10: diferenciadores VNC Pro (experiencia superior a visores clásicos)

Funciones que diferencian a Lucarna de RealVNC Viewer, TigerVNC, Remmina y el visor nativo de macOS, manteniendo el binario ligero y sin dependencias pesadas.

| # | Tarea | Detalle | Tamaño |
|---|---|---|---|
| 10.1 | **Selección de monitor en servidores multipantalla** | Conmutador en la barra de sesión (`Monitor 1`, `Monitor 2`, `Todos`) usando los mensajes de monitor de UltraVNC y/o `ExtendedDesktopSize` (RFC 6143 / TigerVNC), más opción de abrir monitores en ventanas separadas. | M |
| 10.2 | **Capturas de pantalla y grabación ligera de sesión** | Botón de captura instantánea a PNG desde el `Framebuffer` actual. Grabación de sesión guardando el flujo RFB con marcas de tiempo (formato tipo `fbs`, reproducible y auditable con pocos megabytes por hora) para soporte técnico, cumplimiento y evidencia de auditoría. | M |
| 10.3 | **Sesiones en pestañas y vista mosaico (NOC / Wall View)** | Permitir varias sesiones activas en pestañas dentro de `app.rs` y un modo mosaico en vivo de bajo consumo (refresco cada 1–2 s en modo solo ver) para vigilar múltiples máquinas a la vez desde una sala de control y entrar con un clic. | M-L |
| 10.4 | **Portapapeles rico (`CLIP_DIB` y pegado rápido de archivos)** | Extender `Extended Clipboard` para copiar y pegar capturas de pantalla entre local y remoto (validando dimensiones máximas `MAX_DIM` y tamaño descomprimido antes de asignar memoria) y disparar subida de archivos al pegar un archivo en la ventana. | M |
| 10.5 | **Inyección de texto («Type Clipboard»)** | Enviar texto como secuencia de pulsaciones `KeyEvent` cuando el portapapeles remoto no está disponible (pantalla de login de Windows antes de iniciar sesión o servidores sin integración de portapapeles). | XS |

---

## Fase 11: superpoderes sobre SSH y conectividad (RMM sin agente)

Aprovechando que `features/ssh` ya gestiona el cliente `ssh` del sistema, Lucarna puede ofrecer herramientas de administración remota (RMM) sin instalar agentes propietarios en el servidor.

| # | Tarea | Detalle | Tamaño |
|---|---|---|---|
| 11.1 | **Modo «Solo Archivos (SFTP / FT)» desde el Address Book** | Acción en el menú contextual de cada tarjeta para abrir directamente el navegador de archivos de dos columnas (vía SFTP o UltraVNC FT) sin abrir la pantalla remota. Permite usar Lucarna también con servidores Linux sin entorno gráfico. | S |
| 11.2 | **Estado en vivo (Online / Offline) y Wake-on-LAN (WoL)** | Indicador discreto de alcanzabilidad TCP/SSH bajo demanda y envío de paquete mágico UDP (Wake-on-LAN) local o a través del host bastión SSH para encender máquinas apagadas desde la tarjeta. | S |
| 11.3 | **Descubrimiento automático (Tailscale / mDNS)** | Detectar máquinas de la tailnet local (`tailscale status --json` si Tailscale está instalado) y servicios `_rfb._tcp.local` en LAN para mostrarlos listos para conectar sin escribir IPs a mano. | M |
| 11.4 | **Saltos SSH (`ProxyJump`), Cloudflare Tunnel y puertos extra** | Soporte para host bastión (`ssh -J bastion usuario@destino`), integración con `cloudflared access tcp` y reenvío opcional de puertos adicionales (p. ej. abrir el puerto web de administración de un servicio interno de la red remota mientras dura la sesión). | S-M |
| 11.5 | **Telemetría ligera (Health Overlay) y acciones rápidas por SSH** | Si la sesión va por SSH, opción de consultar carga de CPU, RAM, `uptime` y disco con comandos estándar de solo lectura, y ejecutar acciones predefinidas con confirmación explícita (p. ej. reiniciar el servicio VNC remoto si se congela). | M |
| 11.6 | **Sincronización segura de catálogos para equipos (Team Address Book)** | Importar/exportar o sincronizar `connections.txt` mediante un repositorio Git o carpeta compartida entre técnicos de soporte, manteniendo todas las contraseñas aisladas en el llavero local de cada miembro. | S |

### Arquitectura y funcionamiento con Tailscale y Cloudflare Tunnel

![Arquitectura Zero-Trust con Tailscale y Cloudflare Tunnel](diagrams/04-arquitectura-tailscale-cloudflare.png)

![Flujo Paso a Paso con Tailscale y Cloudflare Tunnel](diagrams/05-flujo-integracion-tailscale-cloudflare.png)

```mermaid
flowchart LR
    subgraph CLIENTE["Equipo del Técnico (Lucarna)"]
        LUC["Lucarna Viewer<br/>Address Book + Keychain SO"]
        TS_C["Cliente Tailscale Local<br/>100.x.y.z · MagicDNS<br/>tailscale status --json"]
        CF_C["cloudflared access tcp<br/>Escucha en 127.0.0.1:P1<br/>Token SSO / MFA"]
        SSH_C["Túnel SSH Opcional<br/>features/ssh (-L + SFTP v3)"]
    end

    subgraph RUTA_TS["Opción A: Malla Tailscale / WireGuard"]
        TS_ACL["Plano de Control & ACLs<br/>Solo claves públicas<br/>Nunca ve tus datos"]
        WG_P2P["Túnel P2P WireGuard<br/>ChaCha20-Poly1305<br/>NAT Hole Punching (STUN/DERP)"]
    end

    subgraph RUTA_CF["Opción B: Cloudflare Tunnel + Access"]
        CF_SSO["Cloudflare Access<br/>SSO (Google/Okta/GitHub) + MFA"]
        CF_EDGE["Red Edge Anycast (TLS)<br/>Conector saliente QUIC/HTTP2"]
    end

    subgraph REMOTO["Servidor Remoto (0 Puertos Abiertos en Router)"]
        AG["tailscaled / cloudflared<br/>+ OpenSSH Server"]
        VNC["Servidor VNC<br/>Escuchando SOLO en 127.0.0.1:5900<br/>+ Archivos SFTP v3"]
    end

    LUC --> TS_C & CF_C & SSH_C
    TS_C -.-> TS_ACL
    TS_C <==> WG_P2P <==> AG
    CF_C --> CF_SSO --> CF_EDGE <==> AG
    SSH_C -. "Cifrado E2E sobre A o B" .-> AG
    AG <==> VNC
```

---

## Fase 12: ¿Cómo competir con AnyDesk / TeamViewer de forma prudente y segura?

![Flujo de Funcionamiento y Alternativa Zero-Trust a AnyDesk/TeamViewer](diagrams/02-flujo-funcionamiento-red.png)

```mermaid
flowchart TB
    subgraph M1["Modo 1: Acceso Directo Cifrado (SSH + Loopback)"]
        U1["Técnico + Llavero SO"] -->|"SSH_ASKPASS"| L1["Lucarna (127.0.0.1:P)"]
        L1 ==>|"Túnel SSHv2 Cifrado (Puerto 22)<br/>RFB + UltraVNC FT + SFTP v3"| S1["OpenSSH Server Remoto"]
        S1 -->|"Loopback local"| V1["Servidor VNC (127.0.0.1:5900)<br/>Puerto 5900 cerrado al exterior"]
    end

    subgraph M2["Modo 2: Acceso Desatendido Detrás de NAT (Sin Nube Propia)"]
        U2["Auto-Descubrimiento<br/>(Tailscale / mDNS + WoL)"] --> ZT["Red Zero-Trust / Bastión<br/>(WireGuard P2P · cloudflared · ssh -J)"]
        ZT <==> NOC["Vista Mosaico NOC / Pestañas<br/>+ Selector Multimonitor"]
        NOC <==> FL["Flota de Servidores y PCs<br/>Sin agentes con permisos SYSTEM"]
    end

    subgraph M3["Modo 3: Asistencia Bajo Demanda («QuickSupport Inverso»)"]
        T3["1. Técnico activa<br/>«Esperar Asistencia»"] -->|"Token efímero (1 uso)"| R3["2. Punto de Encuentro Cifrado<br/>(ssh -R inverso o Repetidor Modo II bajo SSH)"]
        C3["3. Cliente Acepta e Inicia<br/>(Conexión saliente, sin abrir router)"] --> R3
        R3 --> A3["4. Sesión + Grabación .fbs<br/>Token revocado al cerrar"]
    end
```

### Por qué NO construir una nube centralizada ni un agente propio con permisos SYSTEM
Herramientas como AnyDesk y TeamViewer funcionan con tres piezas propias: un agente residente con privilegios máximos (`SYSTEM` / `root`), un protocolo propietario y una red mundial de servidores de señalización y relé. **Replicar ese modelo en Lucarna no es prudente** por tres razones:
1. **Riesgo crítico de seguridad:** una infraestructura central de coordinación o un agente propio corriendo como `SYSTEM` convierte a Lucarna en un objetivo de cadena de suministro (como ya les ocurrió a grandes actores del sector).
2. **Coste y mantenimiento 24/7:** operar relés de tráfico de escritorio remoto para terceros exige servidores globales, ancho de banda continuo y moderación contra abuso/malware.
3. **Duplicar lo que ya está auditado:** OpenSSH, WireGuard/Tailscale y Cloudflare Tunnel ya resuelven la criptografía y el paso de NAT con equipos de seguridad dedicados a tiempo completo.

### El camino de Lucarna: experiencia tipo AnyDesk con arquitectura «Zero-Trust sin nube propia»

En lugar de servidores centrales de Lucarna, ofrecemos la misma facilidad de uso apoyándonos en bloques estándar y auditables:

| Caso de uso (estilo AnyDesk/TeamViewer) | Cómo lo resuelve Lucarna de forma segura | Tamaño |
|---|---|---|
| **1. Acceso desatendido sin abrir puertos en el router** | **Integración nativa con SSH + Tailscale / Cloudflare Tunnel:** el servidor VNC escucha solo en `127.0.0.1` y Lucarna conecta por nombre de máquina o túnel SSH con un clic. Cero puertos expuestos a internet y cero servidores intermedios de Lucarna. | S-M |
| **2. Asistencia bajo demanda («QuickSupport» con código/invitación)** | **Modo Escucha Inverso sobre SSH (`Reverse Assist`) o Repetidor cifrado:** el técnico pulsa «Esperar asistencia» en Lucarna (que prepara un túnel inverso efímero en su propio servidor SSH o repetidor privado) y le pasa al cliente un comando o asistente de un clic con un token de un solo uso. Cuando el cliente acepta, la sesión entra cifrada de extremo a extremo y la credencial muere al cerrar. | M-L |
| **3. Evolución a suite multiprotocolo (más allá de VNC)** | Gracias al diseño por rebanadas verticales, el mismo Address Book, llavero de contraseñas, túneles SSH y panel SFTP pueden servir como frontal unificado para conexiones **Solo SFTP/SSH**, **SPICE** (máquinas virtuales Proxmox/QEMU) o **RDP** (vía crate puro Rust a futuro). | L |

---

## Fuera de alcance por ahora

- Infraestructura en la nube propia de relé/señalización (por seguridad y coste; se usa SSH, Tailscale, Cloudflare Tunnel o repetidor propio bajo túnel).
- Codificación "Ultra" y "Ultra2" (propietarias, con compresión LZO); ZRLE y Tight las reemplazan.
- ZYWRLE (con pérdida, poco usada).
- Audio e impresión remota.

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
| 3 | Fase 2: transferencia de archivos ✅ | 2–3 semanas con el spike |
| 4 | Fase 6: empaquetado → **release v0.1** ✅ | 2–3 días |
| 5 | Fase 3: ergonomía ✅ | 1 semana |
| 6 | Fase 4: MS-Logon ✅ | 2–3 días |
| 7 | Fase 1.5: Tight ✅ | 3–4 días |
| 8 | Fase 5: chat y repetidor ✅ | 3–4 días |
| 9 | Fase 7: cifrado (documentación de túneles) ✅ | 1 día |
| 10 | Fase 8: túnel SSH integrado ✅ (8.1) + SFTP (8.2–8.3) | 1–2 semanas |
| 11 | Fase 9: seguridad y endurecimiento transversal | 3–5 días |
| 12 | Fase 10: diferenciadores VNC Pro (multimonitor, pestañas, grabación) | 2–3 semanas |
| 13 | Fase 11: superpoderes sobre SSH y conectividad (SFTP directo, WoL, Tailscale) | 1–2 semanas |
| 14 | Fase 12: asistencia inversa («QuickSupport» sobre SSH) y evolución multiprotocolo | Progresivo |

Las estimaciones son orientativas, para una persona a ritmo sostenido. El mayor riesgo de calendario es la Fase 2, por el protocolo sin documentar y las diferencias entre versiones.
