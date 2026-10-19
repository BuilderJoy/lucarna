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

### macOS
1. Abre el `.dmg` y arrastra **Lucarna** a la carpeta **Aplicaciones**.
2. Abre Lucarna desde Aplicaciones. Como la app todavía no está notarizada por Apple, la primera vez macOS la bloquea:
   - **macOS 15 (Sequoia) o superior:** abre **Ajustes del Sistema → Privacidad y seguridad**, baja hasta el aviso sobre Lucarna y pulsa **Abrir igualmente**.
   - **macOS 14 o anterior:** haz **clic derecho sobre Lucarna → Abrir → Abrir**.

   Solo hace falta la primera vez. Si macOS dice que la app "está dañada", ejecuta en Terminal:
   ```bash
   xattr -dr com.apple.quarantine /Applications/Lucarna.app
   ```
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
- Todavía no se ha probado contra UltraVNC Server en Windows: MS-Logon, chat y descarga de carpetas en zip pueden fallar.
- Las apps no están firmadas por Apple ni por Microsoft, de ahí los avisos al instalar.
