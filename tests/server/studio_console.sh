#!/bin/sh
# Interactive Radio Studio console displayed inside Xvfb :0 for testing Lucarna Host (Phase 1 + Phase 2).

printf '\033[1;36m==============================================================================\033[0m\n'
printf '\033[1;33m   🎙️  LUCARNA STUDIO HOST — CABINA DE RADIO EN VIVO (FASE 1 + FASE 2)       \033[0m\n'
printf '\033[1;36m==============================================================================\033[0m\n'
printf '  • Servidor RFB 3.8 : Binario nativo Lucarna (--host) en espacio de usuario\n'
printf '  • Sin x11vnc       : Captura 64x64 Dirty-Tiles + Compresión ZRLE\n'
printf '  • Sin SSH / sshd   : Servidor de Archivos RFB Msg 7 (Unidades M:, D:, H:)\n'
printf '\033[1;36m------------------------------------------------------------------------------\033[0m\n'
printf '\033[1;32m📂 Contenido inicial en M:\\ (~/Music):\033[0m\n'
find "$HOME/Music" -maxdepth 2 -not -path '*/.*' | sed "s|$HOME/Music|  M:\\\\|g; s|/|\\\\|g"
printf '\033[1;36m------------------------------------------------------------------------------\033[0m\n'
printf '\033[1;35m⌨️  PRUEBA DE TECLADO Y RATÓN (Fase 1):\033[0m\n'
printf '  Haz clic en esta ventana desde el Visor Lucarna, escribe cualquier texto\n'
printf '  y pulsa Enter. Cada línea se guardará también en D:\\teclas_recibidas.txt\n\n'

touch "$HOME/Downloads/teclas_recibidas.txt"

while true; do
    printf '\033[1;33m[Cabina Studio A] > \033[0m'
    if read -r linea; then
        ts=$(date '+%H:%M:%S')
        printf '%s | %s\n' "$ts" "$linea" >> "$HOME/Downloads/teclas_recibidas.txt"
        printf '\033[1;32m  ✓ Recibido en vivo (%s): "%s" (guardado en D:\\teclas_recibidas.txt)\033[0m\n' "$ts" "$linea"
    else
        sleep 1
    fi
done
