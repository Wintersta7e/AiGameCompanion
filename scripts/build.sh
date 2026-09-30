#!/usr/bin/env bash
# Release build of the launcher into release/.
#
# Sets RUSTFLAGS, TARGET_CFLAGS_x86_64_pc_windows_msvc, XWIN_SDK_VERSION and
# XWIN_CRT_VERSION. Clears CARGO_ENCODED_RUSTFLAGS, RUSTC_WRAPPER and
# RUSTC_WORKSPACE_WRAPPER. Reads HOME, XWIN_CACHE_DIR, rust-toolchain.toml,
# scripts/ci-tools.env and cargo's target-dir settings. Every other variable
# is inherited.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1

TARGET="x86_64-pc-windows-msvc"
RELEASE_DIR="release"

# A failed build must not leave an older exe here for the audit to pass.
rm -rf "${RELEASE_DIR}"

# The Windows SDK and CRT cargo xwin downloads and builds against.
XWIN_SDK_VERSION=$(grep -E '^XWIN_SDK_VERSION=' scripts/ci-tools.env | cut -d= -f2-) || XWIN_SDK_VERSION=''
XWIN_CRT_VERSION=$(grep -E '^XWIN_CRT_VERSION=' scripts/ci-tools.env | cut -d= -f2-) || XWIN_CRT_VERSION=''
if [[ -z "${XWIN_SDK_VERSION}" || -z "${XWIN_CRT_VERSION}" ]]; then
	echo "release build: scripts/ci-tools.env lacks XWIN_SDK_VERSION or XWIN_CRT_VERSION" >&2
	exit 1
fi
export XWIN_SDK_VERSION XWIN_CRT_VERSION
echo "release build: XWIN_SDK_VERSION=${XWIN_SDK_VERSION} XWIN_CRT_VERSION=${XWIN_CRT_VERSION} (from scripts/ci-tools.env)"

# A wrapper or encoded flags from the caller would change what gets built.
unset CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER

# Remap the build host's home dir in panic-location strings baked into
# binaries via file!(). Without this, `$HOME/.cargo/...` paths end up in
# every release binary. Assigned, not appended: the caller's flags are not
# part of the release.
#
# Control Flow Guard and CET shadow-stack compatibility. Only Rust code compiled
# with the flag is instrumented: the precompiled std and the C dependencies are
# not.
export RUSTFLAGS="--remap-path-prefix=${HOME}=~ -C control-flow-guard -C link-arg=/CETCOMPAT"

# RUSTFLAGS only covers paths rustc emits. C dependencies bake their own
# __FILE__ paths in: 83 `$HOME/.cargo/...` strings reached a release
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
cargo xwin build -p launcher --release --target "$TARGET" --features custom-protocol --locked

# cargo-xwin reuses a warm cache whatever SDK version is asked for, so the
# pinned SDK must be the one the cache holds.
if [[ ! -d "${XWIN}/sdk/include/${XWIN_SDK_VERSION}" ]]; then
	echo "release build: the xwin cache has no sdk/include/${XWIN_SDK_VERSION}" >&2
	exit 1
fi
echo "release build: xwin sdk/include/${XWIN_SDK_VERSION} present"

# Copy the exe this build wrote: cargo's target directory honours
# CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR and build.target-dir.
TARGET_DIR=$(cargo metadata --no-deps --format-version 1 --locked | jq -r .target_directory)

echo "Copying artifacts to ${RELEASE_DIR}/..."
mkdir -p "$RELEASE_DIR"
cp "${TARGET_DIR}/${TARGET}/release/launcher.exe" "$RELEASE_DIR/"
cp config.example.toml "$RELEASE_DIR/"

echo ""
echo "Done! Release files:"
ls -lh "$RELEASE_DIR/"
