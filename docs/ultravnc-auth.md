# Autenticación de UltraVNC: MS-Logon

Leído del código fuente oficial ([github.com/ultravnc/UltraVNC](https://github.com/ultravnc/UltraVNC), commit `0cd8b41`, 2026-09-28): `rfb/rfbproto.h`, `rfb/dh.cpp`, `vncviewer/ClientConnection.cpp` (`Authenticate`, `AuthMsLogonII`, `AuthMsLogonIII`), `winvnc/winvnc/vncclient.cpp` (`AuthenticateClient`, `AuthMsLogon`, `AuthMsLogonIII`) y `common/d3des.c`.

**Estado:** el cifrado de credenciales está verificado byte a byte contra el código C original (vectores en `mslogon.rs`). El intercambio de claves y el flujo completo están probados contra una implementación del lado servidor hecha en los tests, **no contra un UltraVNC Server real**: no hay forma de levantar uno en Linux o Docker.

## Tipos de seguridad (RFB 3.8)

El servidor manda `n` y luego `n` bytes con los tipos que ofrece. Los relevantes:

| Valor | Nombre | Lo usa |
|---|---|---|
| 1 | None | sin contraseña |
| 2 | VNC auth | contraseña VNC (DES, 8 caracteres) |
| 17 | rfbUltraVNC | marca de servidor UltraVNC; con ella el servidor pide otra ronda (`0xFFFFFFFF`) |
| 0x68 | SCPrompt | el usuario remoto debe aceptar la conexión. **No soportado** |
| 0x71 (113) | MS-Logon II | usuario y contraseña de Windows, DH de 31 bits |
| 0x76 (118) | MS-Logon III | igual, con X25519. Solo en versiones recientes |
| 0x75 | ClientInitExtraMsgSupportNew | extensión del ClientInit. Se ignora |

Un servidor con MS-Logon activado ofrece `[17, 0x75, 0x76, 0x71]`: **siempre anuncia los dos** y el visor oficial elige el III. Sin MS-Logon ofrece `[17, 0x75, 2]` (o `1` si no hay contraseña). Con `RequireMSLogonIII` acepta el II solo para devolver un mensaje de error.

Lucarna elige directamente el tipo final y **no** contesta al 17, así que se salta la ronda extra. Con usuario: III, II, VNC auth, None. Sin usuario: VNC auth, None; si el servidor solo ofrece MS-Logon, falla con "needs a Windows user name".

**RFB 3.3 heredado:** el servidor manda un u32. `0xFFFFFFFA` es MS-Logon II, y después de las credenciales manda el resultado (u32, sin mensaje de error).

## Resultado

Tras la autenticación el servidor manda un u32: `0` = correcto, `0xFFFFFFFF` = sigue otra capa (Lucarna da error), cualquier otro = fallo, seguido (en 3.7+) de `u32 longitud` + texto. El texto es `authentication rejected` si no hay otro.

## MS-Logon II (0x71)

1. S→C: `generador` (8 bytes), `módulo` (8), `clave pública del servidor` (8). Todos u64 **big-endian**.
2. Validación del visor oficial, que Lucarna repite: generador y módulo ≤ 2³¹, generador ≤ módulo, clave pública < 2³¹. Lucarna además exige módulo ≥ 2.
3. C→S: nuestra clave pública `generador^privada mod módulo` (8 bytes, big-endian), con `privada` aleatoria de 31 bits.
4. Clave compartida: `pública_servidor^privada mod módulo`, como u64 big-endian. **Los 8 bytes son la clave DES.**
5. C→S: usuario (256 bytes) y contraseña (64 bytes), cifrados (ver abajo).

## MS-Logon III (0x76)

1. S→C: clave pública X25519 del servidor (32 bytes).
2. C→S: nuestra clave pública X25519 (32 bytes).
3. Secreto compartido X25519 → SHA-256 → **solo los primeros 8 bytes son la clave DES**.
4. C→S: usuario y contraseña cifrados, igual que en el II.

Un secreto compartido todo ceros (clave de baja entropía) se rechaza, como hace libsodium en el visor oficial.

## Cifrado de credenciales (II y III)

`vncEncryptBytes2`: DES en modo CBC. La clave de 8 bytes es **también el IV**, y a cada byte de la clave se le invierte el orden de los bits (la variante de DES de VNC, igual que en VNC auth). Se cifra el buffer de usuario (256 bytes) y luego el de contraseña (64), cada uno empezando de nuevo con el IV.

Usuario: hasta 254 caracteres; contraseña: hasta 63; el resto del buffer en ceros. Texto en la página de códigos ANSI de Windows; Lucarna manda Latin-1 y sustituye lo que no cabe por `?`. Lo que excede el máximo es un error, no se recorta en silencio. Un usuario puede escribirse `DOMINIO\usuario`.

## Seguridad: qué protege y qué no

- **MS-Logon II:** el DH de 31 bits se rompe en segundos con fuerza bruta. Un espía en la red recupera la clave y descifra usuario y contraseña de Windows.
- **MS-Logon III:** el intercambio de claves es fuerte (X25519), **pero la clave que sale es de 8 bytes y cifra con DES**, que tiene 56 bits efectivos y es atacable por fuerza bruta con hardware común. Sigue siendo mucho mejor que el II (hay que capturar el tráfico y romper DES, no solo un DH trivial) pero **no es cifrado fuerte**. El nombre "AES-256" de los comentarios del código de UltraVNC no corresponde a lo que hace.
- **Ninguno cifra la sesión:** pantalla, teclado y archivos viajan sin cifrar.

**Recomendación:** usar siempre un túnel (SSH, WireGuard, Tailscale, Cloudflare Tunnel) cuando la conexión salga de una red de confianza. Con MS-Logon, la contraseña de un usuario de Windows está en juego, no solo la de VNC.
