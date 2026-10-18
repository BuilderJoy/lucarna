#!/bin/sh
# Regenerates the platform icons from assets/logo.svg (macOS: needs rsvg-convert, iconutil and ImageMagick).
set -eu
set_dir=$(mktemp -d)/lucarna.iconset
mkdir -p "$set_dir"
for size in 16 32 128 256 512; do
  rsvg-convert -w $size -h $size assets/logo.svg -o "$set_dir/icon_${size}x${size}.png"
  rsvg-convert -w $((size * 2)) -h $((size * 2)) assets/logo.svg -o "$set_dir/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$set_dir" -o assets/lucarna.icns
magick "$set_dir/icon_256x256.png" -define icon:auto-resize=256,128,64,48,32,24,16 assets/lucarna.ico
