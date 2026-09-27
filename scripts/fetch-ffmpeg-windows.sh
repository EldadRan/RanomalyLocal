#!/usr/bin/env bash
# Fetch LGPL-only static ffmpeg + ffprobe for Windows x64 and place them as Tauri
# sidecars. Meant for CI (windows-latest, git-bash) but runs anywhere with bash,
# curl and unzip.
#
#   scripts/fetch-ffmpeg-windows.sh
#     -> src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe
#     -> src-tauri/binaries/ffprobe-x86_64-pc-windows-msvc.exe
#
# Source: BtbN/FFmpeg-Builds, "win64-lgpl" static variant of the 9.0 release
# branch (same major as the macOS build). BtbN prunes old autobuild-* releases
# after a while; when this URL starts 404ing, either bump TAG/ASSET/SHA256 to a
# current autobuild, or set FFMPEG_WIN_URL to a mirror (e.g. a release asset in
# our own repo) that serves the identical zip - the sha256 check still applies.
set -euo pipefail

TAG="autobuild-2026-09-26-13-03"
ASSET="ffmpeg-n9.0.2-10-g51c4a23d74-win64-lgpl-9.0"
SHA256="ccde5efc9cf6e885cb418f4c0846865f0a7767752d42b1d955b901c41606b205"
URL="${FFMPEG_WIN_URL:-https://github.com/BtbN/FFmpeg-Builds/releases/download/${TAG}/${ASSET}.zip}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${FFMPEG_BUILD_DIR:-$ROOT/.ffmpeg-build}/win64"
OUT="$ROOT/src-tauri/binaries"
TRIPLE="x86_64-pc-windows-msvc"

die() { echo "error: $*" >&2; exit 1; }

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

mkdir -p "$WORK" "$OUT"
zip="$WORK/$ASSET.zip"
if [[ ! -f "$zip" ]]; then
  echo "==> downloading $URL"
  curl -fSL --retry 3 -o "$zip.tmp" "$URL"
  mv "$zip.tmp" "$zip"
fi
[[ "$(sha256 "$zip")" == "$SHA256" ]] || { rm -f "$zip"; die "checksum mismatch for $zip"; }

rm -rf "$WORK/$ASSET"
unzip -q "$zip" -d "$WORK"
for bin in ffmpeg ffprobe; do
  cp "$WORK/$ASSET/bin/$bin.exe" "$OUT/$bin-$TRIPLE.exe"
done
# Ship the licence text that came with the build next to our own notices.
cp "$WORK/$ASSET/LICENSE.txt" "$OUT/ffmpeg-LICENSE-windows.txt" 2>/dev/null || true

echo "==> $OUT/{ffmpeg,ffprobe}-$TRIPLE.exe"
