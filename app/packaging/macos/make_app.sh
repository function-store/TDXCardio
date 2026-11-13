#!/usr/bin/env bash
# Build a universal (Apple Silicon + Intel) TDXCardio.app and zip it.
# Run from app/ on macOS. Output: dist/TDXCardio-macos.zip
set -euo pipefail

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

APP=dist/TDXCardio.app
rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/tdxcardio" \
  target/aarch64-apple-darwin/release/tdxcardio \
  target/x86_64-apple-darwin/release/tdxcardio

# Icon: the same drawing as the window and tray (src/icon.rs).
cargo run --release --example iconset -- dist/rgba
python3 packaging/macos/rgba_to_iconset.py dist/rgba dist/TDXCardio.iconset
iconutil -c icns dist/TDXCardio.iconset -o "$APP/Contents/Resources/TDXCardio.icns"

sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"

# Ad-hoc signature: required for Apple Silicon to run it at all. Not notarized.
codesign --force --deep --sign - "$APP"
codesign --verify --verbose "$APP"

ditto -c -k --keepParent "$APP" dist/TDXCardio-macos.zip
echo "built dist/TDXCardio-macos.zip ($VERSION)"
