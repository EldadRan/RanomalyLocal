# AA Ext

Desktop helper for AAB. AAB writes a job manifest to R2 and opens
`ranomalyext://run?manifest=<presigned URL>`; AA Ext fetches the manifest, asks the user
for what it needs, runs the job locally and shows progress. First tool: `video_to_png`
(download a video, decode it to a PNG sequence).

- Contract AAB must follow: [docs/manifest.md](docs/manifest.md)
- ffmpeg sidecars (LGPL, how to rebuild): [docs/ffmpeg.md](docs/ffmpeg.md)
- Trust settings (R2 account + buckets): `src-tauri/src/config.rs`

## Layout

| Path | What |
|---|---|
| `src-tauri/src/link.rs` | strict `ranomalyext://` parsing, R2 allowlist, presigned expiry |
| `src-tauri/src/manifest.rs` | manifest schema + validation; `op` → tool |
| `src-tauri/src/download.rs` | streaming download, Range resume on drops, size + sha256 check |
| `src-tauri/src/ffmpeg.rs` | ffprobe, explicit YUV→RGB colour plan, ffmpeg progress + cancel |
| `src-tauri/src/disk.rs` | free-space estimate (blocks / warns) |
| `src-tauri/src/job.rs` | state machine the window mirrors; the `video_to_png` job |
| `src/` | the window (vanilla TS, no remote content, strict CSP) |
| `tools/mock_r2.py` | local stand-in for AAB + R2 for end-to-end tests |

Adding a tool: add an `Op` variant in `manifest.rs`, a job function in `job.rs`, and a
view for its options in `src/main.ts`.

## Develop (macOS)

```sh
npm install
scripts/build-ffmpeg-macos.sh arm64        # once; puts sidecars in src-tauri/binaries/
(cd src-tauri && cargo test)               # unit + downloader + real-sidecar decode/colour tests
npx tauri build --debug --bundles app      # deep links only work from a bundle on macOS
lsregister -f "src-tauri/target/debug/bundle/macos/AA Ext.app"   # (full path under LaunchServices.framework)
python3 tools/mock_r2.py --video some.mov --open   # serves manifest + video, opens the link
```

If `clang` complains about the Xcode licence, either run `sudo xcodebuild -license` or
prefix commands with `DEVELOPER_DIR=/Library/Developer/CommandLineTools`.

Debug builds also accept `http://127.0.0.1:<port>/<allowed bucket>/…` so the mock works;
release builds accept only the R2 host in `config.rs`.
