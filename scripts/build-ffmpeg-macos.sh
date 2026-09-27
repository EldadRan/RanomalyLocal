#!/usr/bin/env bash
# Build LGPL-only, statically linked ffmpeg + ffprobe sidecars for macOS.
#
#   scripts/build-ffmpeg-macos.sh arm64     -> src-tauri/binaries/{ffmpeg,ffprobe}-aarch64-apple-darwin
#   scripts/build-ffmpeg-macos.sh x86_64    -> src-tauri/binaries/{ffmpeg,ffprobe}-x86_64-apple-darwin
#   scripts/build-ffmpeg-macos.sh all       -> both of the above + lipo'd *-universal-apple-darwin
#
# No --enable-gpl / --enable-nonfree, no third-party libraries: only libz and the
# macOS system frameworks VideoToolbox needs. See docs/ffmpeg.md.
set -euo pipefail

FFMPEG_VERSION="9.0.2"
FFMPEG_SHA256="8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e"
FFMPEG_URL="https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz"

# nasm is only needed for the x86_64 build (x86 SIMD). Built locally so the
# script does not depend on Homebrew. nasm.us publishes no checksum file; this
# hash was pinned on first download (2026-09-27).
NASM_VERSION="3.02"
NASM_SHA256="87336eba53b4acfe917424ab5d500d2b0054d9f5148d35c2273ccf2cfb712f0d"
NASM_URL="https://www.nasm.us/pub/nasm/releasebuilds/${NASM_VERSION}/nasm-${NASM_VERSION}.tar.xz"

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${FFMPEG_BUILD_DIR:-$ROOT/.ffmpeg-build}"
OUT="$ROOT/src-tauri/binaries"
JOBS="$(sysctl -n hw.ncpu)"

# The exact configure line is recorded in docs/ffmpeg.md; keep the two in sync.
CONFIGURE_FLAGS=(
  --disable-autodetect
  --disable-shared --enable-static
  --disable-debug --disable-doc
  --disable-ffplay
  --disable-network
  --enable-pthreads
  --enable-zlib
  --enable-videotoolbox
)

die() { echo "error: $*" >&2; exit 1; }

fetch() { # url sha256 dest
  local url="$1" sum="$2" dest="$3"
  if [[ ! -f "$dest" ]]; then
    echo "==> downloading $url"
    curl -fSL --retry 3 -o "$dest.tmp" "$url"
    mv "$dest.tmp" "$dest"
  fi
  echo "$sum  $dest" | shasum -a 256 -c - >/dev/null || die "checksum mismatch for $dest"
}

verify_signature() {
  # Optional: FFmpeg signs releases. Verified only if gpg is installed; the pinned
  # sha256 above is the check that always runs.
  command -v gpg >/dev/null || { echo "==> gpg not found; relying on pinned sha256"; return; }
  local gh="$WORK/gnupg"; mkdir -p "$gh"; chmod 700 "$gh"
  curl -fsSL -o "$WORK/ffmpeg-${FFMPEG_VERSION}.tar.xz.asc" "${FFMPEG_URL}.asc"
  curl -fsSL https://ffmpeg.org/ffmpeg-devel.asc | gpg --homedir "$gh" --quiet --import
  gpg --homedir "$gh" --verify "$WORK/ffmpeg-${FFMPEG_VERSION}.tar.xz.asc" \
    "$WORK/ffmpeg-${FFMPEG_VERSION}.tar.xz" || die "GPG signature check failed"
}

ensure_nasm() {
  if command -v nasm >/dev/null; then NASM="$(command -v nasm)"; return; fi
  NASM="$WORK/nasm-install/bin/nasm"
  [[ -x "$NASM" ]] && return
  fetch "$NASM_URL" "$NASM_SHA256" "$WORK/nasm-${NASM_VERSION}.tar.xz"
  rm -rf "$WORK/nasm-${NASM_VERSION}"
  tar -xf "$WORK/nasm-${NASM_VERSION}.tar.xz" -C "$WORK"
  ( cd "$WORK/nasm-${NASM_VERSION}" && ./configure --prefix="$WORK/nasm-install" >/dev/null \
      && make -j"$JOBS" >/dev/null && make install >/dev/null )
}

build_arch() { # arm64 | x86_64
  local arch="$1" triple ffarch extra=()
  case "$arch" in
    arm64)  triple="aarch64-apple-darwin"; ffarch="aarch64" ;;
    x86_64) triple="x86_64-apple-darwin";  ffarch="x86_64"
            ensure_nasm; extra+=(--x86asmexe="$NASM") ;;
    *) die "unknown arch: $arch" ;;
  esac
  if [[ "$(uname -m)" != "$arch" ]]; then extra+=(--enable-cross-compile); fi

  local src="$WORK/src-$arch"
  rm -rf "$src"; mkdir -p "$src"
  tar -xf "$WORK/ffmpeg-${FFMPEG_VERSION}.tar.xz" -C "$src" --strip-components=1

  echo "==> configuring ffmpeg $FFMPEG_VERSION for $arch"
  ( cd "$src" && ./configure \
      --arch="$ffarch" --target-os=darwin \
      --cc="clang -arch $arch" --cxx="clang++ -arch $arch" \
      --extra-cflags="-arch $arch -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET" \
      --extra-ldflags="-arch $arch -mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET" \
      "${CONFIGURE_FLAGS[@]}" ${extra[@]+"${extra[@]}"} > "$WORK/configure-$arch.log" ) \
    || { tail -30 "$src/ffbuild/config.log"; die "configure failed for $arch"; }

  # Refuse to ship anything that turned GPL/nonfree.
  grep -qE '^#define CONFIG_GPL 0' "$src/config.h"     || die "GPL enabled for $arch"
  grep -qE '^#define CONFIG_NONFREE 0' "$src/config.h" || die "nonfree enabled for $arch"

  echo "==> building $arch (make -j$JOBS)"
  ( cd "$src" && make -j"$JOBS" > "$WORK/make-$arch.log" 2>&1 ) \
    || { tail -30 "$WORK/make-$arch.log"; die "make failed for $arch"; }

  mkdir -p "$OUT"
  for bin in ffmpeg ffprobe; do
    cp "$src/$bin" "$OUT/$bin-$triple"
    strip -x "$OUT/$bin-$triple"
  done
  echo "==> $arch done: $OUT/{ffmpeg,ffprobe}-$triple"
}

make_universal() {
  for bin in ffmpeg ffprobe; do
    lipo -create "$OUT/$bin-aarch64-apple-darwin" "$OUT/$bin-x86_64-apple-darwin" \
      -output "$OUT/$bin-universal-apple-darwin"
  done
  echo "==> universal done"
}

main() {
  [[ "$(uname -s)" == "Darwin" ]] || die "macOS only"
  local target="${1:-}"
  [[ "$target" =~ ^(arm64|x86_64|all)$ ]] || die "usage: $0 arm64|x86_64|all"
  mkdir -p "$WORK"
  fetch "$FFMPEG_URL" "$FFMPEG_SHA256" "$WORK/ffmpeg-${FFMPEG_VERSION}.tar.xz"
  verify_signature
  case "$target" in
    all) build_arch arm64; build_arch x86_64; make_universal ;;
    *)   build_arch "$target" ;;
  esac
}

main "$@"
