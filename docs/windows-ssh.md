# Lucarna sobre SSH con Windows (OpenSSH Server + UltraVNC solo en loopback)

Objetivo: que el puerto VNC del equipo Windows **no sea alcanzable desde la red**. Lo único expuesto es SSH (22), y Lucarna lleva por ese túnel tanto la imagen VNC como los archivos (SFTP).

```
Lucarna ──SSH:22──▶ Windows (sshd) ──127.0.0.1:5900──▶ UltraVNC
        └─ SFTP por la misma conexión SSH
```

## 1. Instalar y arrancar OpenSSH Server

En PowerShell **como administrador** (Windows 10 1809+ / Windows 11 / Server 2019+):

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic
# El instalador suele crear la regla; si no existe:
if (-not (Get-NetFirewallRule -Name OpenSSH-Server-In-TCP -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name OpenSSH-Server-In-TCP -DisplayName 'OpenSSH Server (sshd)' `
        -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22
}
```

Comprobar desde el Mac/Linux del técnico: `ssh usuario@equipo-windows` debe pedir contraseña o entrar con la llave.

## 2. Autenticación por llave (recomendado)

En el equipo del técnico: `ssh-keygen -t ed25519` (si aún no tienes llave). Copia el contenido de `~/.ssh/id_ed25519.pub` a Windows:

- **Usuario normal:** `C:\Users\<usuario>\.ssh\authorized_keys`
- **Usuario administrador:** `C:\ProgramData\ssh\administrators_authorized_keys` (Windows ignora el archivo del perfil para administradores). Luego ajusta permisos:

```powershell
icacls "C:\ProgramData\ssh\administrators_authorized_keys" /inheritance:r /grant "Administrators:F" /grant "SYSTEM:F"
```

Si prefieres contraseña, Lucarna la guarda en el llavero del sistema (campo SSH password en Properties) y se la entrega a `ssh` mediante askpass; nunca se escribe en disco.

## 3. Endurecer `sshd_config`

Edita `C:\ProgramData\ssh\sshd_config` y deja, como mínimo:

```
PasswordAuthentication no      # solo si ya usas llaves
PermitRootLogin no
AllowTcpForwarding local       # Lucarna solo usa -L
GatewayPorts no
Subsystem sftp sftp-server.exe # viene por defecto; necesario para archivos
```

Aplica con `Restart-Service sshd`.

## 4. UltraVNC solo en loopback

En **UltraVNC Server → Admin Properties** (o en `ultravnc.ini`, sección `[admin]`):

| Opción | Valor | Efecto |
|---|---|---|
| *Allow Loopback Connections* | ✔ | `AllowLoopback=1`, imprescindible: el túnel llega por `127.0.0.1` |
| *Loopback Only* | ✔ | `LoopbackOnly=1`, el puerto 5900 deja de escuchar en la red |
| *VNC Password* | una contraseña | sigue siendo necesaria (o MS-Logon) |

Reinicia el servicio `uvnc_service`. Si existe una regla de firewall que abre 5900, **elimínala**: ya no hace falta.

Comprobación desde otro equipo de la red: `nc -vz equipo-windows 5900` debe fallar, y `nc -vz equipo-windows 22` debe conectar.

## 5. Conectar desde Lucarna

En **Properties** de la conexión:

- **Address:** `127.0.0.1:5900` (o `localhost`): es la dirección *vista desde Windows*, al final del túnel.
- **SSH:** usuario `usuario` y host `equipo-windows[:22]`.
- **Password:** la contraseña VNC (o usuario/contraseña de Windows con MS-Logon).

Al conectar, la barra de sesión abre **Files** sobre SFTP: el panel remoto empieza en la carpeta del usuario de Windows.

## Problemas frecuentes

| Síntoma | Causa probable |
|---|---|
| `Permission denied (publickey)` siendo administrador | La llave está en `%USERPROFILE%\.ssh` en lugar de `administrators_authorized_keys`, o faltan los permisos `icacls`. |
| `channel 2: open failed: administratively prohibited` | `AllowTcpForwarding no` en `sshd_config`. |
| Conecta por SSH pero VNC dice *connection refused* | UltraVNC no tiene *Allow Loopback Connections* o el servicio no arrancó. |
| `Host key verification failed` | La llave del servidor cambió (reinstalación). Revísala y borra la entrada vieja con `ssh-keygen -R equipo-windows`. |
| Files muestra *SFTP handshake failed* | Falta la línea `Subsystem sftp` o el shell del usuario imprime texto al iniciar sesión. |

## Pruebas automáticas de este escenario

`tests/server/Dockerfile.ssh` reproduce la misma topología en Linux (sshd con llave, x11vnc con `-localhost`). Ver los comandos en `CLAUDE.md` y los tests `live_ssh_tunnel` y `live_sftp_round_trip`.
