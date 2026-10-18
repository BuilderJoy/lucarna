#!/bin/sh
# Builds a universal (Apple Silicon + Intel) Lucarna.app and wraps it in a .dmg under target/macos/.
# Run from the repo root; needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin
set -eu
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --target "$target"
done
out=target/macos
app=$out/Lucarna.app
rm -rf "$out"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$out/dmg"
lipo -create -output "$app/Contents/MacOS/lucarna" \
  target/aarch64-apple-darwin/release/lucarna target/x86_64-apple-darwin/release/lucarna
cp assets/lucarna.icns "$app/Contents/Resources/"
cp LICENSE "$app/Contents/Resources/"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
# Not notarized (that needs an Apple Developer account). An ad-hoc signature keeps
# Apple Silicon from reporting the app as "damaged"; Gatekeeper still asks once.
codesign --force --deep --sign - "$app"
cp -R "$app" "$out/dmg/"
ln -s /Applications "$out/dmg/Applications"
hdiutil create -quiet -volname Lucarna -srcfolder "$out/dmg" -ov -format UDZO "$out/Lucarna-$version-macos-universal.dmg"
echo "$out/Lucarna-$version-macos-universal.dmg"
