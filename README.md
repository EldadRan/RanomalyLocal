# Ranomaly Local

Desktop helper for AAB. AAB writes a job manifest to R2 and opens
`ranomalyext://run?manifest=<presigned URL>`; Ranomaly Local fetches the manifest, asks the user
for what it needs, runs the job locally and shows progress. First tool: `video_to_png`
(download a video, decode it to a PNG sequence).

- Contract AAB must follow: [docs/manifest.md](docs/manifest.md)
- ffmpeg sidecars (LGPL, how to rebuild): [docs/ffmpeg.md](docs/ffmpeg.md)
- Allowlist: `src-tauri/src/config.rs`
- Adding a tool: [docs/adding-an-op.md](docs/adding-an-op.md)

## Layout

**A shell plus ops.** The shell does everything every job needs; each op is one module per side.
Adding one: [docs/adding-an-op.md](docs/adding-an-op.md).

| Path | What |
|---|---|
| `src-tauri/src/config.rs` | **The allowlist**: manifest and media origins (host + path prefix) |
| `src-tauri/src/link.rs` | strict `ranomalyext://` parsing, allowlist matching |
| `src-tauri/src/manifest.rs` | the envelope (`version`, `op`, `job_id`, `title`), fetch, validators for ops |
| `src-tauri/src/job.rs` | the shell: state the window mirrors, start / cancel / keep-or-delete / quit |
| `src-tauri/src/ops/mod.rs` | the op registry and contract (`Job`, `Ctx`, `Events`, `Outcome`) |
| `src-tauri/src/ops/video_to_png.rs` | the first op |
| `src-tauri/src/{download,ffmpeg,disk}.rs` | building blocks ops share |
| `src/main.ts` | the shell's screens: waiting, loading, running, done, partial, error |
| `src/ops/` | one setup screen per op, registered in `index.ts` |
| `src/ui.ts` | shared UI pieces (folder picker, chips, toggle, facts, toast) |
| `tools/mock_r2.py` | local stand-in for AAB + storage for end-to-end runs |
| `tools/preview.html` | every screen in a browser with IPC mocked, for layout checks |

## Develop (macOS)

```sh
npm install
scripts/build-ffmpeg-macos.sh arm64        # once; puts sidecars in src-tauri/binaries/
(cd src-tauri && cargo test)               # unit + downloader + real-sidecar decode/colour tests
npx tauri build --debug --bundles app      # deep links only work from a bundle on macOS
lsregister -f "src-tauri/target/debug/bundle/macos/Ranomaly Local.app"   # (full path under LaunchServices.framework)
python3 tools/mock_r2.py --video some.mov --open   # serves manifest + video, opens the link
```

If `clang` complains about the Xcode licence, either run `sudo xcodebuild -license` or
prefix commands with `DEVELOPER_DIR=/Library/Developer/CommandLineTools`.

Debug builds also accept `http://127.0.0.1:<port>/<allowed bucket>/…` so the mock works;
release builds accept only the origins in `config.rs`.
