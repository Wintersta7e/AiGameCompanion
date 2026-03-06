#!/usr/bin/env bash
set -euo pipefail

TARGET="x86_64-pc-windows-msvc"
RELEASE_DIR="release"
BUILD_DIR="target/${TARGET}/release"

echo "Building overlay + injector (release)..."
cargo xwin build --release --target "$TARGET"

echo "Building launcher frontend..."
(cd crates/launcher && node node_modules/vite/bin/vite.js build)

echo "Building launcher (release)..."
cargo xwin build -p launcher --release --target "$TARGET" --features custom-protocol

echo "Copying artifacts to ${RELEASE_DIR}/..."
mkdir -p "$RELEASE_DIR"
cp "${BUILD_DIR}/overlay.dll"    "$RELEASE_DIR/"
cp "${BUILD_DIR}/injector.exe"   "$RELEASE_DIR/"
cp "${BUILD_DIR}/launcher.exe"   "$RELEASE_DIR/"
cp config.example.toml           "$RELEASE_DIR/"

echo ""
echo "Done! Release files:"
ls -lh "$RELEASE_DIR/"
