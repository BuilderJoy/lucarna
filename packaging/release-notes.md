Visor VNC para macOS, Windows y Linux, compatible con la transferencia de archivos de UltraVNC.

> **Versión {VERSION}, en desarrollo (0.x).** Si algo falla, cuéntalo en [Issues](https://github.com/BuilderJoy/lucarna/issues).

## Descarga

| Sistema | Archivo |
|---|---|
| macOS 11 o superior (Apple Silicon e Intel) | [Lucarna-{VERSION}-macos-universal.dmg](https://github.com/BuilderJoy/lucarna/releases/download/v{VERSION}/Lucarna-{VERSION}-macos-universal.dmg) |
| Windows 10/11 (64 bits) | [Lucarna-{VERSION}-windows-x64-setup.exe](https://github.com/BuilderJoy/lucarna/releases/download/v{VERSION}/Lucarna-{VERSION}-windows-x64-setup.exe) |
| Ubuntu / Debian (64 bits) | [lucarna_{VERSION}_amd64.deb](https://github.com/BuilderJoy/lucarna/releases/download/v{VERSION}/lucarna_{VERSION}_amd64.deb) |
| Otras distribuciones Linux (64 bits) | [lucarna-{VERSION}-linux-x86_64.tar.gz](https://github.com/BuilderJoy/lucarna/releases/download/v{VERSION}/lucarna-{VERSION}-linux-x86_64.tar.gz) |

## Instalación

### macOS (Importante: requiere compilar con Rust por ahora)
> **Nota importante sobre macOS:** Actualmente **no se puede instalar directamente el paquete en Mac** a menos que se ejecute o compile utilizando **Rust**, ya que el proyecto todavía no cuenta con la aprobación y notarización oficial de Apple Developer (Gatekeeper bloquea los binarios sin firma de Apple).

1. **Forma recomendada en macOS (con Rust, sin bloqueos de Gatekeeper):**
   Instala Rust desde [rustup.rs](https://rustup.rs) (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`) y luego:
   ```bash
   git clone https://github.com/BuilderJoy/lucarna.git
   cd lucarna

   # Opción A: Instalar directamente en /Applications (integrado con Spotlight y Launchpad)
   ./packaging/macos/install-local.sh

   # Opción B: Ejecutar directamente en la terminal sin instalar
   cargo run --release
   ```
   *Guía completa y detallada de integración:* [docs/guia-macos-rust.md](docs/guia-macos-rust.md).
2. **Si descargas el `.dmg` precompilado:** macOS lo bloqueará por falta de notarización de Apple. Para saltar el bloqueo manualmente hace falta quitar la cuarentena en Terminal (`xattr -dr com.apple.quarantine /Applications/Lucarna.app`) y autorizar la app en **Ajustes del Sistema → Privacidad y seguridad → Abrir igualmente**.
3. Desinstalar: arrastra Lucarna de Aplicaciones a la Papelera.

### Windows
1. Ejecuta `Lucarna-{VERSION}-windows-x64-setup.exe`.
2. Si aparece **"Windows protegió su PC"** (el instalador aún no está firmado): pulsa **Más información → Ejecutar de todas formas**.
3. Elige instalar **solo para ti** (no pide permisos de administrador) o **para todos los usuarios**, y sigue el asistente. Lucarna queda en el menú Inicio.
4. Desinstalar: **Configuración → Aplicaciones → Lucarna → Desinstalar**.

### Ubuntu / Debian
1. En la carpeta donde descargaste el archivo:
   ```bash
   sudo apt install ./lucarna_{VERSION}_amd64.deb
   ```
2. Lucarna aparece en el menú de aplicaciones.
3. Desinstalar: `sudo apt remove lucarna`.

### Otras distribuciones Linux
```bash
tar -xzf lucarna-{VERSION}-linux-x86_64.tar.gz
./lucarna-{VERSION}-linux-x86_64/lucarna
```
Para guardar contraseñas hace falta un llavero compatible con Secret Service (GNOME Keyring o KWallet).

## Antes de conectarte por internet
VNC no cifra la sesión. Usa siempre un túnel (SSH, VPN o Cloudflare Tunnel) y nunca abras el puerto 5900 a internet. Cómo hacerlo: [Using Lucarna over the internet](https://github.com/BuilderJoy/lucarna#using-lucarna-over-the-internet-always-through-a-tunnel).

## Limitaciones conocidas
- **macOS:** como todavía no se cuenta con la aprobación/notarización de Apple Developer, en Mac no se puede instalar de forma convencional desde el `.dmg` salvo que se compile/ejecute utilizando **Rust** (`cargo run --release`) o se retire manualmente el atributo de cuarentena.
- Todavía no se ha probado contra UltraVNC Server en Windows: MS-Logon, chat y descarga de carpetas en zip pueden fallar.
- En Windows el instalador aún no cuenta con certificado de firma comercial, por lo que SmartScreen muestra el aviso inicial.
