#!/bin/sh
# Compila Lucarna de forma nativa e instala Lucarna.app en /Applications
# No requiere cuenta de Apple Developer ni firma de pago (usa firma ad-hoc local).
set -eu

cd "$(dirname "$0")/../.."

echo "==> Compilando Lucarna en modo release con Rust..."
cargo build --release

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="target/release/Lucarna.app"

echo "==> Empaquetando Lucarna.app (v$version)..."
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/lucarna "$app/Contents/MacOS/lucarna"
cp assets/lucarna.icns "$app/Contents/Resources/"
cp LICENSE "$app/Contents/Resources/"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"

echo "==> Aplicando firma ad-hoc local para macOS..."
codesign --force --deep --sign - "$app"

echo "==> Instalando en /Applications/Lucarna.app..."
rm -rf /Applications/Lucarna.app
cp -R "$app" /Applications/

echo ""
echo "✅ ¡Instalación completada con éxito!"
echo "Lucarna ya está disponible en tu Mac:"
echo " • Puedes abrirla desde Spotlight presionando ⌘ + Espacio y escribiendo 'Lucarna'."
echo " • O desde Launchpad / carpeta Aplicaciones."
