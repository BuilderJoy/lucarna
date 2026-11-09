//! Interface language. The English text in the code is the key; Spanish comes from `ES` below.
//! `tr("Upload")` gives the text in the current language, `fill(tr("{} items"), &[&n])` fills the
//! placeholders (`{}` in order, or `{0}`, `{1}` by position when Spanish needs another order).

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui;

use super::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Es,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Lang::En),
            "es" => Some(Lang::Es),
            _ => None,
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 {
        Lang::Es
    } else {
        Lang::En
    }
}

pub fn set_lang(lang: Lang) {
    LANG.store(u8::from(lang == Lang::Es), Ordering::Relaxed);
}

/// First start: the system language when it is Spanish, English otherwise.
pub fn detect() -> Lang {
    let spanish = |s: &str| {
        s.trim()
            .trim_matches(['(', '"', ','])
            .to_lowercase()
            .starts_with("es")
    };
    for var in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return if spanish(&v) { Lang::Es } else { Lang::En };
        }
    }
    // A macOS app started from the Dock has none of those variables.
    if cfg!(target_os = "macos")
        && let Ok(out) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
        && let Some(first) = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with('"'))
    {
        return if spanish(first) { Lang::Es } else { Lang::En };
    }
    Lang::En
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| ES.iter().copied().collect())
}

/// The text in the current language; anything without a translation stays as written.
pub fn tr(en: &'static str) -> &'static str {
    if lang() == Lang::En {
        return en;
    }
    table().get(en).copied().unwrap_or(en)
}

/// Same for text built at run time (server messages and the like).
pub fn tr_dyn(en: &str) -> String {
    if lang() == Lang::En {
        return en.to_string();
    }
    table()
        .get(en)
        .map_or_else(|| en.to_string(), |s| (*s).to_string())
}

/// Fills `{}` (next argument) and `{N}` (argument N) in `pattern`.
pub fn fill(pattern: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut next = 0;
    let mut rest = pattern;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(j) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let name = &after[..j];
        let index = if name.is_empty() {
            next += 1;
            Some(next - 1)
        } else {
            name.parse::<usize>().ok()
        };
        match index.and_then(|n| args.get(n)) {
            Some(arg) => out.push_str(&arg.to_string()),
            None => out.push_str(&rest[i..i + j + 2]),
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

/// EN | ES switch for the top bar; the choice is remembered across restarts.
pub fn switch(ui: &mut egui::Ui) {
    let mut current = lang();
    let items = [
        (Lang::En, "EN".to_string(), "English"),
        (Lang::Es, "ES".to_string(), "Español"),
    ];
    if theme::segmented(ui, &mut current, &items) {
        choose(ui.ctx(), current);
    }
}

/// Makes `lang` current and remembers it.
pub fn choose(ctx: &egui::Context, lang: Lang) {
    set_lang(lang);
    ctx.data_mut(|d| {
        d.insert_persisted(egui::Id::NULL.with(STORE_KEY), lang.code().to_string());
    });
}

pub const STORE_KEY: &str = "lucarna_lang";

/// Picks the saved language, or detects one, and makes it current.
pub fn init(ctx: &egui::Context) {
    let saved = ctx.data_mut(|d| d.get_persisted::<String>(egui::Id::NULL.with(STORE_KEY)));
    set_lang(
        saved
            .as_deref()
            .and_then(Lang::from_code)
            .unwrap_or_else(detect),
    );
}

/// Spanish text for every English key. Keep `{}` placeholders in the same count as the key.
static ES: &[(&str, &str)] = &[
    (" · +{} queued", " · +{} en cola"),
    (
        "A banner stays on top while anyone is connected.",
        "Un aviso queda siempre encima mientras alguien esté conectado.",
    ),
    (
        "A wrong PIN three times revokes the code.",
        "Tres PIN erróneos y el código se revoca.",
    ),
    ("ADDRESS BOOK", "LIBRETA DE DIRECCIONES"),
    ("Accept error: {}", "Error al aceptar la conexión: {}"),
    ("Active", "Activa"),
    ("Activity", "Actividad"),
    ("Actual size (1:1)", "Tamaño real (1:1)"),
    (
        "Actual size (scroll bars)",
        "Tamaño real (con barras de desplazamiento)",
    ),
    (
        "Add a remote computer or type an address in the top bar to connect.",
        "Agrega un equipo remoto o escribe una dirección en la tarjeta de arriba para conectarte.",
    ),
    ("Address book", "Libreta de direcciones"),
    ("Address book actions", "Acciones de la libreta"),
    (
        "Address, user@host or repeater ID",
        "Dirección, usuario@host o ID del repetidor",
    ),
    ("All", "Todo"),
    ("All connections", "Todas las conexiones"),
    ("AltGr / ES symbols", "AltGr / símbolos ES"),
    (
        "Always on while sharing",
        "Siempre activo mientras compartes",
    ),
    ("Always visible", "Siempre visible"),
    (
        "Ask the other person for their code and PIN",
        "Pide a la otra persona su código y su PIN",
    ),
    (
        "Asking the server for file transfer…",
        "Pidiendo al servidor la transferencia de archivos…",
    ),
    (
        "Assistance session active with {}",
        "Sesión de asistencia activa con {}",
    ),
    (
        "Browse, download and upload files",
        "Explorar, descargar y subir archivos",
    ),
    ("Cancel", "Cancelar"),
    (
        "Chat with the person at the remote machine",
        "Chatear con la persona del equipo remoto",
    ),
    ("Classic VNC password", "Contraseña VNC clásica"),
    ("Clear", "Borrar"),
    ("Clear history", "Borrar historial"),
    ("Clear saved preview image", "Borrar la miniatura guardada"),
    ("Clear search", "Borrar búsqueda"),
    ("Client closed session", "El cliente cerró la sesión"),
    ("Clipboard", "Portapapeles"),
    ("Close", "Cerrar"),
    ("Close session", "Cerrar sesión"),
    ("Connect", "Conectar"),
    ("Connect  {}", "Conectar  {}"),
    ("Connect to a device", "Conectarse a un equipo"),
    ("Connect with a code", "Conectar con un código"),
    ("Connecting", "Conectando"),
    (
        "Connecting to remote desktop…",
        "Conectando con el escritorio remoto…",
    ),
    ("Connection Properties", "Propiedades de la conexión"),
    ("Connection name:", "Nombre de la conexión:"),
    ("Control mouse and keyboard", "Controlar ratón y teclado"),
    ("Controlling", "Controlando"),
    (
        "Copied on the remote machine",
        "Copiado en el equipo remoto",
    ),
    (
        "Copy text on this computer or on the remote machine and it appears here.",
        "Copia texto en este equipo o en el remoto y aparecerá aquí.",
    ),
    ("Copy to your clipboard", "Copiar a tu portapapeles"),
    ("Create", "Crear"),
    ("Create a folder here", "Crear una carpeta aquí"),
    ("Create a new label", "Crear una etiqueta nueva"),
    ("Dark theme", "Tema oscuro"),
    ("Delete", "Eliminar"),
    ("Delete {} {}?", "¿Eliminar {} {}?"),
    ("Desktop", "Escritorio"),
    ("Desktop Preview", "Vista previa del escritorio"),
    ("Direct (LAN)", "Directo (red local)"),
    (
        "Direct address (instead of a code)",
        "Dirección directa (en lugar de un código)",
    ),
    (
        "Disconnect the remote person and stop sharing",
        "Desconectar a la otra persona y dejar de compartir",
    ),
    ("Dismiss message", "Descartar mensaje"),
    (
        "Generated new session code {} and 6-digit E2EE PIN.",
        "Se generó un código de sesión nuevo {} y un PIN E2EE de 6 dígitos.",
    ),
    ("The PIN was changed.", "Se cambió el PIN."),
    ("{} asks to connect.", "{} pide conectarse."),
    (
        "Connection from {} was not allowed.",
        "No se permitió la conexión de {}.",
    ),
    (
        "Sharing stopped: nobody connected in 15 minutes.",
        "Se dejó de compartir: nadie se conectó en 15 minutos.",
    ),
    ("{} wants to connect", "{} quiere conectarse"),
    (
        "The PIN was right. Allow only someone you expect.",
        "El PIN era correcto. Permite solo a alguien que esperas.",
    ),
    ("No files", "Sin archivos"),
    ("Allow", "Permitir"),
    ("Deny", "Denegar"),
    (
        "Refused automatically in {} s.",
        "Se rechaza automáticamente en {} s.",
    ),
    (
        "You are sharing this computer. Nobody has connected yet.",
        "Estás compartiendo este equipo. Nadie se ha conectado aún.",
    ),
    (
        "Off. Nothing can be sent or fetched.",
        "Desactivado. No se puede enviar ni traer nada.",
    ),
    ("Off", "Desactivado"),
    (
        "Not encrypted: anyone on the network can read the screen. Use it only on a network you trust.",
        "Sin cifrar: cualquiera en la red puede ver la pantalla. Úsalo solo en una red de confianza.",
    ),
    (
        "Anyone on your network can reach this port. They still need the PIN and your OK.",
        "Cualquiera en tu red puede llegar a este puerto. Aun así necesita el PIN y tu permiso.",
    ),
    ("{}  Search", "{}  Buscar"),
    (
        "Ready. Nothing is shared until you start.",
        "Listo. No se comparte nada hasta que empieces.",
    ),
    ("Start sharing  {}", "Empezar a compartir  {}"),
    (
        "The code and PIN only work while you are sharing.",
        "El código y el PIN solo funcionan mientras compartes.",
    ),
    ("How they reach you", "Cómo se conectan contigo"),
    ("Code", "Código"),
    ("Copy invitation", "Copiar invitación"),
    (
        "Ask the other person to copy their invitation, then paste it here with {}. Or type the code and PIN.",
        "Pide a la otra persona que copie su invitación y pégala aquí con {}. O escribe el código y el PIN.",
    ),
    (
        "Invitation read. Check it and press Connect.",
        "Invitación leída. Revísala y pulsa Conectar.",
    ),
    ("Duplicate", "Duplicar"),
    (
        "Don't send keyboard, mouse or clipboard to the server",
        "No enviar teclado, ratón ni portapapeles al servidor",
    ),
    ("Double-click to connect", "Doble clic para conectar"),
    (
        "Double-click to connect · Right-click for options",
        "Doble clic para conectar · Clic derecho para opciones",
    ),
    ("Double-click to go up", "Doble clic para subir un nivel"),
    ("Double-click to open", "Doble clic para abrir"),
    (
        "Download the folder as a .zip",
        "Descargar la carpeta como .zip",
    ),
    ("Download to your computer", "Descargar a tu equipo"),
    ("Downloads", "Descargas"),
    ("Drive", "Unidad"),
    (
        "Drop files on this window to upload",
        "Suelta archivos en esta ventana para subirlos",
    ),
    ("Encrypted end to end", "Cifrado de extremo a extremo"),
    ("End session now", "Terminar sesión ahora"),
    ("End-to-end encrypted", "Cifrado de extremo a extremo"),
    ("Ended", "Terminada"),
    ("Everything", "Todo"),
    ("Export", "Exportar"),
    ("Export connections", "Exportar conexiones"),
    (
        "Exported {} connection(s) to {}",
        "Se exportaron {} conexión(es) a {}",
    ),
    ("FULL", "PANT. COMPLETA"),
    (
        "Failed to bind Host port {}: {}",
        "No se pudo abrir el puerto {} del host: {}",
    ),
    (
        "Failed to reach repeater {}: {}",
        "No se pudo contactar al repetidor {}: {}",
    ),
    (
        "Failed to register ID:{} on repeater: {}",
        "No se pudo registrar ID:{} en el repetidor: {}",
    ),
    ("Favorites", "Favoritos"),
    ("File path to export to:", "Ruta del archivo a exportar:"),
    ("File path to import from:", "Ruta del archivo a importar:"),
    ("Files", "Archivos"),
    ("Files (SFTP)", "Archivos (SFTP)"),
    ("Filesystem root", "Raíz del sistema de archivos"),
    ("Fit", "Ajustar"),
    ("Fit to window", "Ajustar a la ventana"),
    ("Friendly Name", "Nombre"),
    ("From remote", "Del remoto"),
    ("Full screen", "Pantalla completa"),
    (
        "Generate a new code and PIN",
        "Generar un código y PIN nuevos",
    ),
    (
        "Generated new session code {} and 4-digit E2EE PIN.",
        "Se generó un código de sesión nuevo {} y un PIN E2EE de 4 dígitos.",
    ),
    ("Grid view", "Vista de cuadrícula"),
    ("Hidden connections", "Conexiones ocultas"),
    ("Hide", "Ocultar"),
    ("History", "Historial"),
    ("Home", "Inicio"),
    (
        "Home folder: files you upload land here",
        "Carpeta de inicio: aquí llegan los archivos que subes",
    ),
    ("Import", "Importar"),
    ("Import connections", "Importar conexiones"),
    (
        "Imported {} connection(s) from {}",
        "Se importaron {} conexión(es) desde {}",
    ),
    (
        "Input & Session Behavior",
        "Entrada y comportamiento de la sesión",
    ),
    ("Invalid folder name.", "Nombre de carpeta no válido."),
    ("Invalid name.", "Nombre no válido."),
    ("Join with code", "Unirse con código"),
    ("Just now", "Ahora mismo"),
    ("LABELS", "ETIQUETAS"),
    (
        "Label name (e.g. Cambridge, London, Servers):",
        "Nombre de la etiqueta (p. ej. Oficina, Servidores, Casa):",
    ),
    ("Labels", "Etiquetas"),
    ("Last connected", "Última conexión"),
    (
        "Let someone connect to this computer",
        "Permite que alguien se conecte a este equipo",
    ),
    (
        "Let someone connect to this computer with Studio Host.",
        "Permite que alguien se conecte a este equipo con Studio Host.",
    ),
    ("Let someone help you", "Deja que alguien te ayude"),
    ("Light theme", "Tema claro"),
    ("List view", "Vista de lista"),
    ("Listen on", "Escuchar en"),
    ("Local disk", "Disco local"),
    (
        "Locked after {} failed PIN attempts from {}. Regenerate code to resume.",
        "Bloqueado tras {} intentos de PIN fallidos desde {}. Genera un código nuevo para continuar.",
    ),
    (
        "Lucarna QuickSupport Invitation\nRepeater: {}\nSession Code: {} (ID:{})\nE2EE PIN: {}",
        "Invitación de Lucarna QuickSupport\nRepetidor: {}\nCódigo de sesión: {} (ID:{})\nPIN E2EE: {}",
    ),
    (
        "Lucarna Studio E2EE Invitation\nAddress: {}\nSession Code: {}\nE2EE PIN: {}",
        "Invitación de Lucarna Studio E2EE\nDirección: {}\nCódigo de sesión: {}\nPIN E2EE: {}",
    ),
    (
        "Lucarna Studio Host\nAddress: {}\nPassword: {}",
        "Lucarna Studio Host\nDirección: {}\nContraseña: {}",
    ),
    (
        "Lucarna creates a one-time code and PIN. Give both to the person you trust. Nothing is shared until you start.",
        "Lucarna crea un código y un PIN de un solo uso. Dáselos a la persona de confianza. No se comparte nada hasta que empieces.",
    ),
    ("Modified (UTC)", "Modificado (UTC)"),
    (
        "Move yours and remote input pauses for 2 seconds.",
        "Mueve el tuyo y la entrada remota se pausa 2 segundos.",
    ),
    ("Name", "Nombre"),
    ("Name (A–Z)", "Nombre (A–Z)"),
    ("Network", "Red"),
    ("Never", "Nunca"),
    ("New Connection", "Nueva conexión"),
    ("New connection", "Nueva conexión"),
    ("New folder:", "Carpeta nueva:"),
    ("New label", "Etiqueta nueva"),
    ("New label…", "Etiqueta nueva…"),
    (
        "No answer from the server. File transfer may be disabled in its UltraVNC settings.",
        "El servidor no respondió. Puede que la transferencia de archivos esté desactivada en su configuración de UltraVNC.",
    ),
    (
        "No connections in this view",
        "No hay conexiones en esta vista",
    ),
    (
        "No labels yet.\nRight-click a desktop or click + to organize by location, team or role.",
        "Aún no hay etiquetas.\nHaz clic derecho en un equipo o pulsa + para organizar por lugar, equipo o rol.",
    ),
    ("Not encrypted", "Sin cifrar"),
    ("Not shared", "Sin compartir"),
    ("Nothing here yet", "Aún no hay nada"),
    (
        "Nothing running. Pick a file on the left and press Upload, or double-click a file to send or fetch it.",
        "Nada en curso. Elige un archivo a la izquierda y pulsa Subir, o haz doble clic en un archivo para enviarlo o traerlo.",
    ),
    ("Offline", "Sin conexión"),
    ("Online", "En línea"),
    ("Only these folders", "Solo estas carpetas"),
    ("Password", "Contraseña"),
    ("Paused", "En pausa"),
    ("Paused when you move", "Se pausa cuando mueves el ratón"),
    ("Pick a drive", "Elige una unidad"),
    ("Picture & Scaling", "Imagen y escala"),
    ("Properties", "Propiedades"),
    (
        "Properties (Alt+Enter) · Right-click card for all options",
        "Propiedades (Alt+Intro) · Clic derecho en la tarjeta para ver todas las opciones",
    ),
    ("Quick toggle:", "Acceso rápido:"),
    ("Recently connected", "Conectadas recientemente"),
    ("Reconnect to {}", "Reconectar con {}"),
    ("Refresh local folder", "Actualizar la carpeta local"),
    ("Refresh remote folder", "Actualizar la carpeta remota"),
    ("Remote", "Remoto"),
    ("Removable", "Extraíble"),
    ("Rename", "Renombrar"),
    ("Rename connection", "Renombrar conexión"),
    ("Rename label", "Renombrar etiqueta"),
    ("Rename label \"{}\" to:", "Renombrar la etiqueta \"{}\" a:"),
    ("Rename {} to:", "Renombrar {} a:"),
    ("Repeater", "Repetidor"),
    ("Repeater (through NAT)", "Repetidor (a través de NAT)"),
    (
        "Repeater E2EE session finished.",
        "La sesión E2EE por repetidor terminó.",
    ),
    (
        "Repeater or direct address",
        "Repetidor o dirección directa",
    ),
    (
        "Require the PIN and encrypt the session (recommended)",
        "Exigir el PIN y cifrar la sesión (recomendado)",
    ),
    ("Root /", "Raíz /"),
    (
        "SECURITY ALERT: Failed E2EE PIN attempt #{} from {}.",
        "ALERTA DE SEGURIDAD: intento fallido de PIN E2EE n.º {} desde {}.",
    ),
    (
        "SSH and a repeater can't be combined: clear one of them",
        "SSH y un repetidor no se pueden combinar: borra uno de los dos",
    ),
    ("SSH encrypted", "Cifrado por SSH"),
    ("SSH password", "Contraseña SSH"),
    ("SSH tunnel", "Túnel SSH"),
    ("SSH user", "Usuario SSH"),
    ("Save", "Guardar"),
    ("Save & Connect", "Guardar y conectar"),
    (
        "Save desktop thumbnail preview when connected",
        "Guardar la miniatura del escritorio al conectar",
    ),
    (
        "Save this address to your Address Book",
        "Guardar esta dirección en tu libreta",
    ),
    ("Saved to {}", "Guardado en {}"),
    ("Screen", "Pantalla"),
    ("Screen shared", "Pantalla compartida"),
    ("See my screen", "Ver mi pantalla"),
    (
        "Select a file below to upload (or double-click it)",
        "Elige abajo un archivo para subir (o haz doble clic en él)",
    ),
    (
        "Select a remote folder first",
        "Elige primero una carpeta remota",
    ),
    (
        "Send '{}' to remote computer",
        "Enviar '{}' al equipo remoto",
    ),
    (
        "Send Ctrl+Alt+Del to the remote machine",
        "Enviar Ctrl+Alt+Supr al equipo remoto",
    ),
    (
        "Send key combinations and AltGr symbols",
        "Enviar combinaciones de teclas y símbolos AltGr",
    ),
    (
        "Send text to the remote clipboard",
        "Enviar texto al portapapeles remoto",
    ),
    (
        "Send this text to the remote computer",
        "Enviar este texto al equipo remoto",
    ),
    ("Sent from this computer", "Enviado desde este equipo"),
    ("Server", "Servidor"),
    ("Server address", "Dirección del servidor"),
    ("Session active with {}", "Sesión activa con {}"),
    ("Session closed: {}", "Sesión cerrada: {}"),
    ("Session code", "Código de sesión"),
    ("Session ended with {}: {}", "Sesión terminada con {}: {}"),
    (
        "Session established with {} ({}).",
        "Sesión establecida con {} ({}).",
    ),
    (
        "Share clipboard text with remote computer",
        "Compartir el texto del portapapeles con el equipo remoto",
    ),
    ("Share this device", "Compartir este equipo"),
    ("Share this device ●", "Compartir este equipo ●"),
    ("Share this screen", "Compartir esta pantalla"),
    ("Sharing", "Compartiendo"),
    ("Sharing is locked", "El uso compartido está bloqueado"),
    (
        "Sharing this computer · waiting on {}",
        "Compartiendo este equipo · esperando en {}",
    ),
    ("Shortcut", "Acceso directo"),
    ("Show", "Mostrar"),
    ("Show / hide password", "Mostrar / ocultar contraseña"),
    ("Size", "Tamaño"),
    (
        "Start session in full screen mode",
        "Iniciar la sesión en pantalla completa",
    ),
    ("Stop sharing", "Dejar de compartir"),
    ("Stop waiting", "Dejar de esperar"),
    ("Synced", "Sincronizado"),
    ("Test Connection", "Probar conexión"),
    ("Testing connection…", "Probando la conexión…"),
    (
        "That folder can't be read on the server.",
        "Esa carpeta no se puede leer en el servidor.",
    ),
    (
        "The code ends when you stop waiting. Three wrong PINs revoke it.",
        "El código termina cuando dejas de esperar. Tres PIN erróneos lo revocan.",
    ),
    (
        "The repeater only relays bytes it cannot read.",
        "El repetidor solo retransmite bytes que no puede leer.",
    ),
    (
        "The server refused file transfer.",
        "El servidor rechazó la transferencia de archivos.",
    ),
    ("Three tries, then locked", "Tres intentos y se bloquea"),
    ("To remote", "Al remoto"),
    ("Toggle favorite", "Marcar o quitar favorito"),
    ("Transfer files", "Transferir archivos"),
    ("Transfers", "Transferencias"),
    ("Type and press Enter", "Escribe y pulsa Intro"),
    ("Type or paste text…", "Escribe o pega texto…"),
    (
        "UltraVNC file transfer",
        "Transferencia de archivos UltraVNC",
    ),
    ("Upload", "Subir"),
    (
        "Upload the selected file to the remote folder",
        "Subir el archivo seleccionado a la carpeta remota",
    ),
    ("Upload to the remote folder", "Subir a la carpeta remota"),
    ("Uploaded {}", "Subido {}"),
    ("Username", "Usuario"),
    ("VIEW ONLY", "SOLO VER"),
    ("VNC Server", "Servidor VNC"),
    ("Verified E2EE PIN", "PIN E2EE verificado"),
    ("Via repeater", "Por repetidor"),
    (
        "View and manage synchronized clipboard text",
        "Ver y gestionar el texto sincronizado del portapapeles",
    ),
    ("View only", "Solo ver"),
    (
        "View only (do not send keyboard, mouse or clipboard)",
        "Solo ver (no enviar teclado, ratón ni portapapeles)",
    ),
    ("Volumes", "Volúmenes"),
    ("Wait for connection  {}", "Esperar conexión  {}"),
    ("Waiting", "Esperando"),
    (
        "Waiting for remote connection on {} ({}).",
        "Esperando una conexión remota en {} ({}).",
    ),
    (
        "Waiting for the other person",
        "Esperando a la otra persona",
    ),
    (
        "Waiting for the remote screen…",
        "Esperando la pantalla remota…",
    ),
    ("What they can do", "Qué pueden hacer"),
    ("You stay in control", "Tú mantienes el control"),
    ("Your mouse always wins", "Tu ratón siempre tiene prioridad"),
    ("Your mouse wins", "Tu ratón tiene prioridad"),
    (
        "comma-separated, e.g. Cambridge, London",
        "separadas por comas, p. ej. Oficina, Servidores",
    ),
    ("connection closed", "conexión cerrada"),
    ("file", "el archivo"),
    ("folder", "la carpeta"),
    (
        "host, host:display or host::port (ID:1234 behind a repeater)",
        "host, host:pantalla o host::puerto (ID:1234 tras un repetidor)",
    ),
    (
        "leave empty to use your SSH keys or agent",
        "déjalo vacío para usar tus claves SSH o el agente",
    ),
    ("nobody connected yet", "nadie conectado aún"),
    (
        "optional UltraVNC repeater, e.g. repeater.example.com:5901",
        "repetidor UltraVNC opcional, p. ej. repetidor.ejemplo.com:5901",
    ),
    (
        "optional host[:port]: encrypts VNC and files through SSH",
        "host[:puerto] opcional: cifra VNC y archivos mediante SSH",
    ),
    ("optional, e.g. Office PC", "opcional, p. ej. PC de oficina"),
    (
        "optional, for MS-Logon (Windows login)",
        "opcional, para MS-Logon (inicio de sesión de Windows)",
    ),
    (
        "optional, the account on the SSH server (e.g. vboxuser)",
        "opcional, la cuenta en el servidor SSH (p. ej. vboxuser)",
    ),
    ("repeater host:port", "repetidor host:puerto"),
    ("user@host[:port]", "usuario@host[:puerto]"),
    ("via {}", "vía {}"),
    ("{}  Add to Favorites", "{}  Añadir a favoritos"),
    ("{}  All connections", "{}  Todas las conexiones"),
    ("{}  Clear desktop preview", "{}  Borrar la vista previa"),
    ("{}  Connect (View only)", "{}  Conectar (solo ver)"),
    ("{}  Connect\tEnter", "{}  Conectar\tIntro"),
    ("{}  Copy invitation", "{}  Copiar invitación"),
    ("{}  Delete label", "{}  Eliminar etiqueta"),
    ("{}  Delete\t⌫", "{}  Eliminar\t⌫"),
    ("{}  Duplicate\t{}", "{}  Duplicar\t{}"),
    ("{}  End session now", "{}  Terminar sesión ahora"),
    ("{}  Export connections…", "{}  Exportar conexiones…"),
    ("{}  Favorites", "{}  Favoritos"),
    ("{}  Generate a new code", "{}  Generar un código nuevo"),
    ("{}  Hidden", "{}  Ocultas"),
    ("{}  Hide", "{}  Ocultar"),
    ("{}  Import connections…", "{}  Importar conexiones…"),
    ("{}  Invitation copied", "{}  Invitación copiada"),
    ("{}  Labels", "{}  Etiquetas"),
    ("{}  New connection", "{}  Nueva conexión"),
    ("{}  New label…", "{}  Etiqueta nueva…"),
    ("{}  Online", "{}  En línea"),
    ("{}  Properties…\tAlt+Enter", "{}  Propiedades…\tAlt+Intro"),
    ("{}  Recent", "{}  Recientes"),
    ("{}  Remove from Favorites", "{}  Quitar de favoritos"),
    ("{}  Rename label…", "{}  Renombrar etiqueta…"),
    ("{}  Rename…\tF2", "{}  Renombrar…\tF2"),
    (
        "{}  Search connections, labels or addresses",
        "{}  Buscar conexiones, etiquetas o direcciones",
    ),
    ("{}  Show in Address Book", "{}  Mostrar en la libreta"),
    ("{}  This device", "{}  Este equipo"),
    ("{} (Copy)", "{} (copia)"),
    ("{} Add connection here", "{} Añadir conexión aquí"),
    ("{} Bell", "{} Timbre"),
    ("{} Connect", "{} Conectar"),
    ("{} Copy", "{} Copiar"),
    ("{} Disconnect", "{} Desconectar"),
    (
        "{} End-to-end encrypted. The repeater relays the session but cannot read it, and the PIN never leaves this computer.",
        "{} Cifrado de extremo a extremo. El repetidor retransmite la sesión pero no puede leerla, y el PIN nunca sale de este equipo.",
    ),
    ("{} Folder", "{} Carpeta"),
    ("{} Keys", "{} Teclas"),
    ("{} New connection", "{} Nueva conexión"),
    ("{} Options & Display", "{} Opciones y pantalla"),
    ("{} Pin to Favorites", "{} Fijar en favoritos"),
    ("{} Properties", "{} Propiedades"),
    ("{} Reconnect ({})", "{} Reconectar ({})"),
    ("{} Send", "{} Enviar"),
    ("{} Send to remote", "{} Enviar al remoto"),
    ("{} Upload", "{} Subir"),
    ("{} chars · Latin-1", "{} caracteres · Latin-1"),
    ("{} items", "{} elementos"),
    ("{} of {} online", "{} de {} en línea"),
    ("{} shared folders", "{} carpetas compartidas"),
    (
        "{} {} in the session pastes straight to the remote",
        "{} {} en la sesión pega directo en el equipo remoto",
    ),
    ("{}d ago", "hace {} d"),
    ("{}h ago", "hace {} h"),
    ("{}m ago", "hace {} min"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_in_order_and_by_index() {
        assert_eq!(fill("{} of {}", &[&1, &2]), "1 of 2");
        assert_eq!(fill("{1} de {0}", &[&1, &2]), "2 de 1");
        assert_eq!(fill("sin {} marcador", &[]), "sin {} marcador");
    }

    /// Text that reads the same in Spanish, key names and product names.
    const SAME: &[&str] = &[
        "Alt+↵",
        "Chat",
        "Control",
        "Ctrl K",
        "Ctrl+Alt+Del",
        "Ctrl+D",
        "Ctrl+Shift+F",
        "Ctrl+V",
        "E2EE ChaCha20-Poly1305",
        "Lucarna",
        "Lucarna Studio Host",
        "Local",
        "CD-ROM",
        "PIN",
        "RFB 3.8",
        "SSH",
        "VNC Auth",
        "{} Chat",
        "{} General",
        "{} SSH",
        "\r\n",
        "⌘N / Ctrl+N",
    ];

    fn unescape(s: &str) -> String {
        s.replace("\\n", "\n")
            .replace("\\t", "\t")
            .replace("\\r", "\r")
            .replace("\\\"", "\"")
    }

    #[test]
    fn every_translated_text_in_the_views_has_a_spanish_entry() {
        let sources = [
            include_str!("../app.rs"),
            include_str!("../features/connection/view.rs"),
            include_str!("../features/host/view.rs"),
            include_str!("../features/file_transfer/view.rs"),
            include_str!("../features/clipboard/view.rs"),
            include_str!("../features/chat/view.rs"),
            include_str!("theme.rs"),
        ];
        let mut missing = Vec::new();
        for src in sources {
            let mut rest = src;
            while let Some(i) = rest.find("tr(\"") {
                let after = &rest[i + 4..];
                let mut end = 0;
                let bytes = after.as_bytes();
                while end < bytes.len() {
                    if bytes[end] == b'\\' {
                        end += 2;
                    } else if bytes[end] == b'"' {
                        break;
                    } else {
                        end += 1;
                    }
                }
                let key = unescape(&after[..end]);
                if !SAME.contains(&key.as_str()) && !table().contains_key(key.as_str()) {
                    missing.push(key);
                }
                rest = &after[end..];
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "no Spanish text for: {missing:?}");
    }

    #[test]
    fn spanish_entries_keep_their_placeholders() {
        for (en, es) in ES {
            assert_eq!(en.matches("{}").count(), es.matches("{}").count(), "{en}");
        }
    }

    #[test]
    fn detect_reads_a_spanish_locale() {
        assert!("es_MX.UTF-8".to_lowercase().starts_with("es"));
        assert_eq!(Lang::from_code("es"), Some(Lang::Es));
        assert_eq!(Lang::from_code("xx"), None);
    }
}
