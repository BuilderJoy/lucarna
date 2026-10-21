# Guía de Instalación e Integración de Lucarna en macOS con Rust

Esta guía explica paso a paso cómo compilar, ejecutar e integrar **Lucarna** como una aplicación nativa en tu MacBook (Apple Silicon M1/M2/M3/M4 o Intel) utilizando **Rust**.

---

## ¿Por qué se requiere Rust en macOS actualmente?

Lucarna se encuentra en fase de desarrollo activo (0.x) y **aún no cuenta con certificado de firma de código ni notarización de pago de Apple Developer**. 

En macOS, cualquier archivo descargado de internet sin firma de Apple (como los `.dmg` de GitHub Releases) es bloqueado por **Apple Gatekeeper**, mostrando alertas de seguridad ("*No se puede abrir porque Apple no puede comprobar si contiene software malicioso*").

> [!TIP]
> **La ventaja de usar Rust:**  
> Cuando compilas el proyecto localmente en tu propia máquina con Rust, macOS reconoce el binario como creado por el usuario local (de confianza). No se le aplica el atributo de cuarentena de red (`com.apple.quarantine`), por lo que **se ejecuta con total fluidez y sin bloqueos de Gatekeeper**.

---

## 1. Requisitos Previos

Solo necesitas tener instalado Rust y las herramientas básicas de compilación de macOS (Command Line Tools):

1. **Instalar Rust (si aún no lo tienes):**
   Abre la Terminal y ejecuta:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
   *(Presiona Enter para la instalación por defecto. Al terminar, cierra y vuelve a abrir la Terminal o ejecuta `source "$HOME/.cargo/env"`).*

2. **Verificar herramientas de línea de comandos de macOS:**
   ```bash
   xcode-select --install
   ```
   *(Si ya las tienes instaladas, el sistema te avisará que ya están presentes).*

---

## 2. Forma 1: Integración Completa en macOS como `Lucarna.app` (Recomendada)

Lucarna incluye un script automatizado que compila el código fuente para tu procesador (ARM64 para M1/M2/M3/M4 o x86_64 para Intel), le añade el icono nativo, aplica una firma local ad-hoc y lo coloca directamente en tu carpeta de `/Applications`.

1. Clona el repositorio y entra a la carpeta:
   ```bash
   git clone https://github.com/BuilderJoy/lucarna.git
   cd lucarna
   ```

2. Ejecuta el script de instalación local:
   ```bash
   ./packaging/macos/install-local.sh
   ```

3. **¡Listo! Integración inmediata en tu MacBook:**
   - **Spotlight:** Pulsa `⌘ + Espacio`, escribe `Lucarna` y presiona Enter.
   - **Launchpad:** Aparecerá con su icono oficial junto a tus demás aplicaciones.
   - **Dock:** Puedes arrastrar `Lucarna.app` desde `/Applications` a tu Dock para tenerlo siempre a mano.

---

## 3. Forma 2: Ejecución Directa en Desarrollo (Sin instalar)

Si prefieres ejecutar Lucarna directamente desde el código fuente sin instalarla en el sistema:

```bash
git clone https://github.com/BuilderJoy/lucarna.git
cd lucarna
cargo run --release
```

La primera vez descargará las dependencias y compilará; las siguientes ejecuciones iniciarán en menos de 1 segundo.

---

## 4. Forma 3: Instalación como Comando de Terminal Global (CLI)

Si sueles trabajar en la terminal y quieres abrir Lucarna escribiendo simplemente `lucarna`:

```bash
cd lucarna
cargo install --path .
```

Una vez instalado, puedes ejecutar desde cualquier directorio:
```bash
lucarna
```

*(Opcional) Si quieres crear un alias rápido en tu shell (`~/.zshrc`):*
```bash
echo 'alias lucarna="cd ~/Workspace/lucarna && cargo run --release -q"' >> ~/.zshrc
source ~/.zshrc
```

---

## 5. ¿Y si ya descargaste el archivo `.dmg` precompilado de Releases?

Si ya descargaste el `.dmg` de GitHub y arrastraste `Lucarna.app` a `/Applications`, macOS lo bloqueará al abrirlo por falta de firma de Apple. Puedes habilitarlo manualmente con **un solo comando en la Terminal**:

```bash
xattr -dr com.apple.quarantine /Applications/Lucarna.app
```

O bien desde la interfaz gráfica:
1. Ve a **Ajustes del Sistema → Privacidad y seguridad**.
2. Desplázate hacia abajo hasta la sección **Seguridad**.
3. Verás el aviso *"Se bloqueó el uso de Lucarna..."*; haz clic en **Abrir igualmente** e introduce tu contraseña o Touch ID.

---

## Resumen de Comandos Rápidos para tu MacBook

```bash
# 1. Obtener el código
git clone https://github.com/BuilderJoy/lucarna.git
cd lucarna

# 2. Instalarlo como app nativa en /Applications (Spotlight + Dock)
./packaging/macos/install-local.sh
```
