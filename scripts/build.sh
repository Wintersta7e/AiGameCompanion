#!/usr/bin/env bash
set -euo pipefail

TARGET="x86_64-pc-windows-msvc"
RELEASE_DIR="release"
BUILD_DIR="target/${TARGET}/release"

# Strip the build host's home dir from panic-location strings baked into
# binaries via file!(). Without this, `/home/<user>/.cargo/...` paths leak
# into every binary on the release artifacts.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${HOME}=~"

# RUSTFLAGS only covers paths rustc emits. C dependencies bake their own
# __FILE__ paths in: 83 `/home/<user>/.cargo/...` strings reached a release
# binary through aws-lc-sys (pulled in by rustls) before this.
#
# aws-lc-sys applies this same remap itself, but skips it when the compiler is
# cl-like (builder/cc_builder.rs) -- which is exactly our case, since the MSVC
# target builds through clang-cl. It only reads its own prefixed variables, not
# CFLAGS, and clang-cl needs the `/clang:` prefix to accept a GNU-style flag.
#
# aws-lc-sys does not append: whichever CFLAGS variable it finds first REPLACES
# `CFLAGS_<target>`, which is where cargo-xwin puts the Windows SDK include
# paths -- passing the flag alone drops them and the build dies on a missing
# `stdlib.h`. So the sysroot flags are repeated here alongside it. If cargo-xwin
# changes them, this build fails loudly on a missing header rather than
# silently shipping the paths again.
XWIN="${XWIN_CACHE_DIR:-${HOME}/.cache/cargo-xwin}/xwin"
export TARGET_CFLAGS_x86_64_pc_windows_msvc="--target=x86_64-pc-windows-msvc \
-Wno-unused-command-line-argument -fuse-ld=lld-link \
/imsvc ${XWIN}/crt/include /imsvc ${XWIN}/sdk/include/ucrt \
/imsvc ${XWIN}/sdk/include/um /imsvc ${XWIN}/sdk/include/shared \
/imsvc ${XWIN}/sdk/include/winrt \
/clang:-ffile-prefix-map=${HOME}=~"

echo "Building launcher frontend..."
(cd crates/launcher && node node_modules/vite/bin/vite.js build)

echo "Building launcher (release)..."
cargo xwin build -p launcher --release --target "$TARGET" --features custom-protocol

echo "Copying artifacts to ${RELEASE_DIR}/..."
mkdir -p "$RELEASE_DIR"
cp "${BUILD_DIR}/launcher.exe" "$RELEASE_DIR/"
cp config.example.toml "$RELEASE_DIR/"

echo ""
echo "Done! Release files:"
ls -lh "$RELEASE_DIR/"
