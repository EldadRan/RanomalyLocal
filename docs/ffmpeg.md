# ffmpeg sidecars

The app shells out to `ffmpeg` and `ffprobe`, bundled as Tauri sidecars
(`bundle.externalBin`). The binaries are never committed: `src-tauri/binaries/`
and `.ffmpeg-build/` are gitignored and produced by the scripts below.

## Why LGPL only

FFmpeg is LGPL by default and becomes GPL only when built with `--enable-gpl`
(needed for x264/x265 and a few filters) and non-redistributable with
`--enable-nonfree`. We only *decode* video and *encode* PNG, both native to
FFmpeg, so neither is needed. Staying LGPL means we can ship a closed-source app
as long as we meet the LGPL notice/source obligations below. A GPL ffmpeg would
pull the whole app under GPL distribution terms.

Homebrew's ffmpeg is built `--enable-gpl --enable-shared` with dozens of dylibs:
fine for local experiments, never bundle it.

## macOS: built from source

```
scripts/build-ffmpeg-macos.sh arm64    # Apple Silicon
scripts/build-ffmpeg-macos.sh x86_64   # Intel (cross-compiled, nasm built locally if missing)
scripts/build-ffmpeg-macos.sh all      # both + lipo'd universal
```

Takes ~2.5 min for `all` on an M-series Mac. Outputs
`src-tauri/binaries/{ffmpeg,ffprobe}-{aarch64,x86_64,universal}-apple-darwin`.

- Source: `https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz`,
  sha256 `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e`
  (pinned in the script; GPG signature is additionally checked when `gpg` is installed).
- Deployment target: macOS 11.0 (`MACOSX_DEPLOYMENT_TARGET` overrides).
- The script aborts if the generated `config.h` has `CONFIG_GPL` or `CONFIG_NONFREE` set.
- If Xcode's licence has not been accepted, compilers refuse to run. Either accept it
  (`sudo xcodebuild -license`) or build against the Command Line Tools:
  `DEVELOPER_DIR=/Library/Developer/CommandLineTools scripts/build-ffmpeg-macos.sh all`.

Configure line (per arch; `$ARCH` = arm64|x86_64, `$FFARCH` = aarch64|x86_64):

```
./configure --arch=$FFARCH --target-os=darwin \
  --cc="clang -arch $ARCH" --cxx="clang++ -arch $ARCH" \
  --extra-cflags="-arch $ARCH -mmacosx-version-min=11.0" \
  --extra-ldflags="-arch $ARCH -mmacosx-version-min=11.0" \
  --disable-autodetect --disable-shared --enable-static \
  --disable-debug --disable-doc --disable-ffplay --disable-network \
  --enable-pthreads --enable-zlib --enable-videotoolbox \
  [x86_64 only: --x86asmexe=<nasm>]  [when not the host arch: --enable-cross-compile]
```

Result: LGPL v2.1+, linked only against `/usr/lib/libSystem`, `/usr/lib/libz` and
the CoreFoundation/CoreMedia/CoreServices/CoreVideo/VideoToolbox frameworks.
All native decoders (ProRes, H.264, HEVC, DNxHD, AV1, …) and the PNG encoder are
in; `-hwaccel videotoolbox` is available.

These binaries must be codesigned with hardened runtime and notarized together
with the app (phase 2).

## Windows: prebuilt LGPL static build

```
scripts/fetch-ffmpeg-windows.sh     # runs in CI (windows-latest, git-bash)
```

Downloads BtbN/FFmpeg-Builds `ffmpeg-n9.0.2-10-g51c4a23d74-win64-lgpl-9.0.zip`
from release `autobuild-2026-09-26-13-03`, checks its sha256, and copies
`ffmpeg`/`ffprobe` to `src-tauri/binaries/*-x86_64-pc-windows-msvc.exe`.

Caveats:
- BtbN prunes old `autobuild-*` releases. When the pinned one disappears, bump the
  tag/asset/sha256, or mirror the zip as a release asset in our repo and point
  `FFMPEG_WIN_URL` at it (recommended before relying on CI long-term).
- This build is `--enable-version3`, i.e. **LGPL v3**, not 2.1 (v3 because some of
  its external libs are Apache-2.0). Same obligations, v3 wording; note the
  anti-tivoisation clause is irrelevant for a desktop app.
- It links ~43 external libraries, so each exe is ~128 MB. If installer size matters,
  replace this with our own minimal cross-compiled (mingw) LGPL build using the same
  flags as macOS.

## Licensing obligations (do these before distributing)

1. **Notice**: the app's About / Licenses screen states that it includes FFmpeg,
   licensed under the LGPL (v2.1+ on macOS, v3 on Windows), with the full licence
   text shipped with the app (`LICENSE.txt` from the Windows zip is copied to
   `src-tauri/binaries/ffmpeg-LICENSE-windows.txt`; for macOS use `COPYING.LGPLv2.1`
   from the source tarball).
2. **Source**: link to the exact source used — the tarball URL + sha256 above, the
   configure line, and our build script (or host a copy of the tarball ourselves).
   For Windows, link the BtbN release tag, which publishes its build scripts.
3. **Replaceability**: ffmpeg runs as a separate executable, not linked into our
   binary, so users can swap it; don't obfuscate or checksum-lock the sidecar in a
   way that prevents that.
4. Don't claim FFmpeg endorses the product, and don't use the FFmpeg logo.
