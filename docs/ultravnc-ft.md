# Protocolo de transferencia de archivos de UltraVNC

UltraVNC no publica una especificación. Este documento sale de leer el código fuente oficial ([github.com/ultravnc/UltraVNC](https://github.com/ultravnc/UltraVNC), commit `0cd8b41`, 2026-09-28):

- `rfb/rfbproto.h`: constantes y struct.
- `winvnc/winvnc/vncclient.cpp`: servidor (`case rfbFileTransfer`, `filetransferrequestPart1/2`, `SendFileChunk`, `FinishFileSending`).
- `vncviewer/FileTransfer.cpp`: visor oficial (`ProcessFileTransferMsg`, `RequestPermission`, `RequestRemoteFile`, `ReceiveFile`, `ReceiveFileChunk`).

**Estado:** leído del código, **todavía sin confirmar con tráfico real**. Las marcas ⚠ son puntos que hay que verificar contra un servidor.

## Mensaje

Un solo tipo de mensaje RFB, **7**, en las dos direcciones. Cabecera de 12 bytes, big-endian como el resto de RFB:

| Offset | Tipo | Campo | Uso |
|---|---|---|---|
| 0 | u8 | `type` | siempre 7 |
| 1 | u8 | `contentType` | qué es (tabla abajo) |
| 2 | u16 | `contentParam` | subtipo o flags |
| 4 | u32 | `size` | tamaño, flag o código de error según el caso |
| 8 | u32 | `length` | bytes de datos que siguen |

Siguen `length` bytes de datos. **Excepción:** `FileHeader` del servidor lleva 4 bytes más después de los datos (ver Descarga).

| contentType | Nombre | Dirección |
|---|---|---|
| 1 | DirContentRequest | C→S |
| 2 | DirPacket | S→C |
| 3 | FileTransferRequest (descarga) | C→S |
| 4 | FileHeader | ambas |
| 5 | FilePacket | ambas |
| 6 | EndOfFile | ambas |
| 7 | AbortFileTransfer | ambas; también pide permiso |
| 8 | FileTransferOffer (subida) | C→S |
| 9 | FileAcceptHeader | S→C |
| 10 | Command | C→S |
| 11 | CommandReturn | S→C |
| 12 | FileChecksums (transferencia delta) | ambas |
| 14 | FileTransferAccess (respuesta de permiso) | S→C |
| 15 / 16 | FileTransferSessionStart / End | C→S |
| 17 | FileTransferProtocolVersion | S→C |

## Activación y versión

1. **Pseudo-encoding `0xFFFF8002` (FTProtocolVersion).** Si el cliente lo incluye en `SetEncodings`, el servidor:
   - responde con un mensaje 7, `contentType=17`, `contentParam` = versión (hoy 4) y `size` = tamaño de bloque (32768);
   - manda los archivos en bloques de 32 KiB en vez de 8 KiB;
   - acepta rutas en UTF-8 y listados Unicode.

   Un servidor viejo ignora el encoding desconocido. Versiones: 1 = ≤RC18, 2 = base, 3 = mensajes de sesión, 4 = Unicode y `ADirInaccessible`.
2. **Permiso.** El visor oficial manda `AbortFileTransfer` (7) con `contentParam = 3` (cualquier valor distinto de 0), `size = 0`, `length = 0`. El servidor responde:
   - `contentType = 14` (FileTransferAccess), `size = 1` → permitido;
   - `contentType = 14`, `size = 0xFFFFFFFF` → denegado.

   Se deniega si `EnableFileTransfer=0`, si las entradas remotas están deshabilitadas o si falla la impersonación de usuario (`FTUserImpersonation`).
3. **Sesión** (versión ≥3): `SessionStart` (15) al abrir el panel y `SessionEnd` (16) al cerrarlo, con todos los campos en 0. Son informativos.

**⚠ Silencio:** cuando la transferencia está deshabilitada, el servidor lee y **descarta** los pedidos de listado y descarga sin responder nada. El cliente necesita un timeout propio.

## Listar unidades

C→S: `contentType=1`, `contentParam=2` (RDrivesList), `size=0`, `length=0`.

S→C: `contentType=2`, `contentParam=3` (ADrivesList), `length=n` y n bytes en grupos de 4: `"C:l\0D:c\0…"`. El tercer carácter es el tipo:

| Carácter | Tipo |
|---|---|
| `l` | disco local |
| `f` | extraíble |
| `c` | CD-ROM |
| `n` | red |

Es la salida de `GetLogicalDriveStrings` con la `\` reemplazada por el tipo. n ≤ 256.

## Listar un directorio

C→S: `contentType=1`, `contentParam = 1 | 0x8000` (RDirContent + bandera Unicode), `length=n` y la ruta. La ruta tiene la forma `C:\carpeta\`, con la barra final, porque el servidor le agrega `*`. Va en UTF-8 si se pidió Unicode, o en la página de códigos ANSI si no. Máximo `MAX_PATH*3+1` bytes; si es más larga, el servidor la descarta sin responder.

Hay atajos: las rutas que empiezan con `My Documents`, `Desktop` o `Network Favorites` (opcionalmente envueltas en `[ … ]`) se resuelven a esas carpetas del usuario conectado.

S→C, una secuencia de mensajes `contentType=2`:

1. **Ruta resuelta:** `contentParam = 1 (| 0x8000)` y como datos la ruta completa sin el `*`. Este primer paquete **no** trae `WIN32_FIND_DATA`.
2. **Una entrada por archivo o carpeta**, incluido `..` y excluido `.`. `contentParam` es 1 (carpeta) o 2 (archivo), `| 0x8000` si es Unicode.

   **⚠ En la rama ANSI el servidor manda siempre `contentParam=1`, también para archivos.** Hay que distinguir por el atributo `FILE_ATTRIBUTE_DIRECTORY` (0x10).
3. **Fin:** `contentParam=0`, `length=0`.

Si la carpeta no se puede leer, en lugar de todo lo anterior llega un solo mensaje con `contentParam=18` (ADirInaccessible) y `length=0`. Un cliente viejo lo trata como fin del listado.

### Formato de cada entrada

Es un `WIN32_FIND_DATA` truncado, en little-endian (memoria de Windows):

| Offset | Tipo | Campo |
|---|---|---|
| 0 | u32 | `dwFileAttributes` (0x10 = carpeta) |
| 4 | u64 (2×u32, parte baja primero) | `ftCreationTime` |
| 12 | u64 | `ftLastAccessTime` |
| 20 | u64 | `ftLastWriteTime` (FILETIME: intervalos de 100 ns desde 1601-01-01 UTC) |
| 28 | u32 | `nFileSizeHigh` |
| 32 | u32 | `nFileSizeLow` |
| 36 | u32 | `dwReserved0` (`0xFFFFFFFE` = subcarpeta inaccesible) |
| 40 | u32 | `dwReserved1` |
| 44 | … | `cFileName` |

El nombre depende de la rama:

- **Unicode (`0x8000`):** UTF-16LE terminado en `\0\0`. `length = 44 + (chars+1)*2`.
- **ANSI:** bytes en la página de códigos del servidor, **sin** terminador. `length = 44 + strlen`.

## Descargar un archivo

1. C→S: `contentType=3`, `contentParam=0`, `size=1` para pedir compresión (el visor oficial la pide por debajo de 2 Mbit/s), `length=n` y la ruta completa del archivo, en UTF-8 si el servidor habla v4. Para descargar una carpeta, el nombre va como `[ nombre ]`: el servidor la comprime en un zip con prefijo `!UVNCDIR-`.
2. S→C, `FileHeader`: `contentType=4`, `size` = parte baja del tamaño y `length=n`. Después:
   - n bytes con `"ruta,MM/DD/YYYY HH:MM"` (fecha de modificación en UTC);
   - **4 bytes extra fuera de `length`:** u32 big-endian con la parte alta del tamaño.

   `size = sizeH = 0xFFFFFFFF` significa error: el archivo no existe o está bloqueado. En ese caso no sigue nada.
3. C→S, `FileHeader` de respuesta: `contentType=4`, `size` = el mismo `size` para aceptar o `0xFFFFFFFF` para rechazar, `length=0`.

   Antes de aceptar, el cliente puede mandar `FileChecksums` (12) con adler32 por bloque para hacer una transferencia delta. Lucarna no lo hace.
4. S→C, `FilePacket` repetidos: `contentType=5`, `length` = bytes que siguen y `size` según el bloque:
   - `size=0`: datos sin comprimir;
   - `size=1`: un bloque zlib independiente (`compress()`, **no** es un flujo persistente) que infla a como mucho un bloque (32 KiB, u 8 KiB sin v4);
   - `size=2`: el bloque "ya está" en el destino (solo pasa con delta) y no trae datos.
5. S→C, cierre:
   - `EndOfFile` (6): terminó bien;
   - `AbortFileTransfer` (7): error de lectura del lado del servidor.

   El cliente puede cancelar mandando `AbortFileTransfer`.

Mientras dura la descarga siguen llegando `FramebufferUpdate` entre los paquetes: el lector de sesión tiene que despachar todo.

## Pendiente de documentar (antes de implementar la escritura)

- Subida: `FileTransferOffer` → `FileAcceptHeader` → `FilePacket` → `EndOfFile`, con el prefijo de archivo parcial `!UVNCPFT-`.
- Comandos `Command` (10) y `CommandReturn` (11): crear, borrar y renombrar.

## Seguridad en el cliente

- Acotar `length` antes de reservar memoria: listados de 64 KiB como máximo y paquetes de archivo de 32 KiB + 1024.
- Inflar con salida acotada al tamaño de bloque.
- Sanear el nombre de destino local: solo el último componente; nada de `..`, `/`, `\`, `:` ni rutas absolutas. Nunca escribir fuera de la carpeta elegida.
- Para descargas, escribir a un archivo temporal y renombrarlo al terminar.
