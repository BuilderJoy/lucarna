# Test VNC server: x11vnc on a virtual X display with an xterm, with UltraVNC file transfer on.
FROM alpine:3.20
RUN apk add --no-cache x11vnc xvfb xterm && mkdir /srv/ft && \
    printf 'lucarna file transfer test\n' > /srv/ft/hello.txt && \
    head -c 100000 /dev/urandom > /srv/ft/random.bin
CMD Xvfb :0 -screen 0 1024x768x24 & sleep 1; DISPLAY=:0 xterm -bg navy & \
    x11vnc -display :0 -passwd "${VNC_PASSWORD:-secret}" -forever -shared -quiet -ultrafilexfer
