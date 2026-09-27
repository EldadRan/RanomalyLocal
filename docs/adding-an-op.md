# Adding an op

An **op** is one tool the helper can run: `video_to_png` today. The calling app chooses it with the
manifest's `op` field. Each op is one Rust module and one TypeScript module. Everything around
them is shared, and an op gets it without writing any of it:

- the link and its checks
- manifest fetching
- one job at a time
- the running, done, partial and error screens
- progress, Cancel and sleep prevention
- "keep or delete" after a failure
- quitting when done

## The contract

```
app link ─► shell fetches manifest ─► ops::prepare(envelope, doc)  ── picks the op by `op`
                                          │
                        Job::from_manifest(doc)   parse + validate the op's own fields
                                          │
  Ready screen ◄── Job::details() ── src/ops/<op>.ts draws the setup screen from it
       │  user choices = `opts` (the op's own JSON shape)
       ├─ invoke("preflight", {opts}) ─► Job::preflight   live feedback (disk space, …)
       └─ invoke("start", {opts})     ─► Job::start_check  refuse before anything runs
                                          │
                                   Job::run(ctx, opts) ─► Outcome
                                     ctx.stage(i)          moves the running screen
                                     ctx.reporter(i, unit) progress (bytes | frames | items)
                                     ctx.cancel            honour it promptly
                                          │
               Done { facts, warning, open }  │  Partial { message, facts, discard, open }  │  Failed
```

| Piece | Where | What it owns |
|---|---|---|
| Rust module | `src-tauri/src/ops/<op>.rs` | Its manifest fields and validation, `details`, the options it accepts, `preflight`, `start_check`, `run`, `STAGES` |
| Registration | `src-tauri/src/ops/mod.rs` | One `Job` variant and one arm per `match`; the compiler lists every place |
| Setup screen | `src/ops/<op>.ts` | An `OpUI.ready(ctx)` returning the screen: job facts, the user's choices, Cancel and Start |
| Registration | `src/ops/index.ts` | One line mapping the op name to its UI |
| Contract | `docs/api.md` | A section in §4 with the op's fields and what the user sees |

## Rules an op must keep

- **Validate every manifest field.** Read fields with `manifest::fields` so errors name the
  field. URLs go through `manifest::media_url` (https only),
  file names through `manifest::check_filename`, and display strings through
  `manifest::display_text`. The manifest comes from the network, so treat it as hostile.
- **Honour `ctx.cancel`** at every await that can take a while. Use `tokio::select!` on
  `ctx.cancel.cancelled()`, or pass the token down, the way `download::download` and
  `ffmpeg::decode` do.
- **Report only what you wrote.** `Discard` lists the files the op created. The shell deletes
  exactly those, then removes any listed folders only if they are empty.
- **Paths the user can open come back in `Outcome.open`.** The webview never passes a path to
  Rust.
- **Subprocesses:** use `ffmpeg::command(name)` for bundled sidecars: arguments as an array, no
  shell, no console window on Windows, and killed on drop. A new sidecar goes in `externalBin`
  in `tauri.conf.json`.
- **Needs a new Tauri permission?** Prefer doing the work in Rust. The webview's capability stays
  `core:event:default`.

## Reusable pieces

| Need | Use |
|---|---|
| Download a file with resume and hash check | `download::download` |
| Probe a video / decode it | `ffmpeg::probe`, `ffmpeg::plan`, `ffmpeg::decode` |
| Free-space check | `disk::check` (video-shaped today; generalise when a second op needs it) |
| Labelled results | `Fact::text`, `Fact::mono` |
| UI: folder picker, chips, toggle, fact grid, toast | `src/ui.ts` |

## Checklist

1. `ops/<op>.rs`, with unit tests for its manifest parsing and an end-to-end test driving
   `run` through a recording `Events` (see `video_to_png::e2e`).
2. Add the variant and its match arms in `ops/mod.rs`.
3. `src/ops/<op>.ts` plus one line in `src/ops/index.ts`.
4. A section in `docs/api.md` §4, and new messages added to its §5.
5. Add a state for it to `tools/preview.html` and check the screen at 460 px.
6. `cargo test`, `npm run build`, then a real run from a bundle with `tools/mock_r2.py`, or a
   mock of your own.

An older helper that receives a manifest for an op it doesn't know shows *"this request needs a
newer version of Ranomaly Local"*. That's why ops can ship on a calling app's side before every user has
updated.
