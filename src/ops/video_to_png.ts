// Setup screen for `video_to_png` (src-tauri/src/ops/video_to_png.rs).

import { bytes, call, chips, duration, fact, facts, folderPicker, h, label, toggle } from "../ui";
import type { OpUI, ReadyContext } from "./index";

interface Details {
  filename: string;
  size: number;
  expires_at: number | null;
  frames: number;
  width: number;
  height: number;
  fps: number | null;
  pix_fmt: string | null;
  source_high_bit: boolean;
  source_alpha: boolean;
}

type Depth = "eight" | "sixteen";

interface Options { frames_parent: string; keep_video: boolean; video_dir: string | null; depth: Depth }

interface DiskCheck {
  verdict: "ok" | "warn" | "block";
  frames_low: number;
  frames_high: number;
  video: number;
  volumes: { path: string; free: number }[];
}

export const videoToPng: OpUI = {
  ready(ctx: ReadyContext): Node[] {
    const d = ctx.details as Details;
    const opts: Options = { frames_parent: "", keep_video: false, video_dir: null, depth: "eight" };

    // ---- what the job is
    const expiry = h("dd", { class: "num" });
    const tick = () => {
      if (!d.expires_at) { expiry.textContent = "—"; return; }
      const left = d.expires_at - Date.now() / 1000;
      expiry.textContent = left > 0 ? `in ${duration(left)}` : "expired — start the job again";
      expiry.className = left > 0 ? (left < 600 ? "num warn" : "num") : "error";
    };
    tick();
    const timer = window.setInterval(tick, 1000);
    ctx.onLeave(() => clearInterval(timer));

    const info = facts([
      fact("File", d.filename),
      fact("Size", bytes(d.size), true),
      fact("Frames", `${d.frames.toLocaleString()} · ${d.width}×${d.height}${d.fps ? ` · ${d.fps} fps` : ""}`, true),
      d.pix_fmt && fact("Format", d.pix_fmt, true),
    ]);
    info.append(h("dt", {}, "Link"), expiry);
    info.append(h("dt", {}, "From"), h("dd", { class: "num" }, ctx.source));
    if (ctx.jobId) info.append(h("dt", {}, "Job"), h("dd", { class: "num muted" }, ctx.jobId));

    // ---- the user's choices
    const frames = folderPicker("Where should the frames go?", (p) => {
      opts.frames_parent = p;
      if (opts.keep_video && !opts.video_dir) { opts.video_dir = p; video.set(p); }
      refresh();
    });
    const video = folderPicker("Where should the video be saved?", (p) => { opts.video_dir = p; refresh(); });
    const videoField = h("div", { class: "field", hidden: true }, label("Video folder"), video.el);
    const keep = toggle("keep", (on) => {
      opts.keep_video = on;
      videoField.hidden = !on;
      if (on && !opts.video_dir && opts.frames_parent) { opts.video_dir = opts.frames_parent; video.set(opts.video_dir); }
      refresh();
    });
    const alpha = d.source_alpha ? " + ALPHA" : "";
    const depth = chips<Depth>([["eight", `8-BIT${alpha}`], ["sixteen", `16-BIT${alpha}`]], opts.depth,
      (v) => { opts.depth = v; refresh(); });

    // ---- disk check and Start (absent, not disabled, until the choices can run)
    const disk = h("p", { class: "disk", hidden: true });
    const start = h("button", { class: "primary", hidden: true, onclick: async () => {
      start.hidden = true;
      await call("start", { opts });
    } }, "Start");
    let seq = 0;
    async function refresh() {
      const mine = ++seq;
      start.hidden = true;
      if (!opts.frames_parent || (opts.keep_video && !opts.video_dir)) { disk.hidden = true; return; }
      const c = await call<DiskCheck>("preflight", { opts });
      if (mine !== seq || !c) return;
      disk.hidden = false;
      disk.className = `disk ${c.verdict}`;
      const need = `Needs ${bytes(c.frames_low + c.video)}–${bytes(c.frames_high + c.video)}`;
      const free = c.volumes.map((v) => bytes(v.free)).join(" / ");
      disk.textContent =
        c.verdict === "block" ? `${need} · only ${free} free — choose another folder`
        : c.verdict === "warn" ? `${need} · ${free} free — may run out of space`
        : `${need} · ${free} free`;
      start.hidden = c.verdict === "block";
    }

    return [
      h("h1", {}, ctx.title),
      h("div", { class: "card" }, info),
      h("div", { class: "card" },
        h("div", { class: "field" }, label("Frames folder"), frames.el),
        h("label", { class: "row", htmlFor: "keep" }, h("span", { class: "small dim" }, "Keep the downloaded video"), keep),
        videoField,
        h("div", { class: "row" }, label("PNG bit depth"), depth),
        disk),
      h("div", { class: "actions" },
        h("button", { class: "secondary", onclick: () => call("dismiss") }, "Cancel"),
        start),
    ];
  },
};
