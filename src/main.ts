import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---------------------------------------------------------------- types (mirror src-tauri/src/job.rs)

type Stage = "download" | "probe" | "decode" | "cleanup";
type Depth = "match" | "eight" | "sixteen";

interface Manifest {
  op: "video_to_png";
  job_id: string | null;
  title: string;
  input: { filename: string; size: number; sha256: string | null };
  params: { frames: number; width: number; height: number; fps: number | null; pix_fmt: string | null };
  input_expires_at: number | null;
}

type View =
  | { state: "idle" }
  | { state: "loading" }
  | { state: "ready"; manifest: Manifest; source_high_bit: boolean; source_alpha: boolean }
  | { state: "running"; title: string; stage: Stage }
  | { state: "partial"; title: string; message: string; written: number; frames_dir: string }
  | { state: "done"; title: string; frames_dir: string; frames: number; expected: number; video: string | null; warning: string | null }
  | { state: "failed"; message: string };

interface Progress { stage: Stage; done: number; total: number }

interface DiskCheck {
  verdict: "ok" | "warn" | "block";
  frames_low: number;
  frames_high: number;
  video: number;
  volumes: { path: string; free: number; need_low: number; need_high: number }[];
}

interface StartOptions { frames_parent: string; keep_video: boolean; video_dir: string | null; depth: Depth }

// ---------------------------------------------------------------- helpers

const app = document.getElementById("app")!;
const toastEl = document.getElementById("toast")!;

/** Builds elements with textContent only — manifest strings never become HTML. */
function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> & { class?: string } = {},
  ...children: (Node | string | null | false)[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  const { class: cls, ...rest } = props;
  if (cls) el.className = cls;
  Object.assign(el, rest);
  for (const c of children) if (c !== null && c !== false) el.append(c);
  return el;
}

function bytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < units.length - 1) { n /= 1000; i++; }
  return `${n.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

function duration(s: number): string {
  if (!isFinite(s) || s < 0) return "—";
  s = Math.round(s);
  const hh = Math.floor(s / 3600), mm = Math.floor((s % 3600) / 60), ss = s % 60;
  return hh ? `${hh}h ${mm}m` : mm ? `${mm}m ${ss}s` : `${ss}s`;
}

let toastTimer = 0;
function toast(msg: string) {
  toastEl.textContent = msg;
  toastEl.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toastEl.classList.remove("show"), 5000);
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    toast(String(e));
    return undefined;
  }
}

// ---------------------------------------------------------------- views

let view: View = { state: "idle" };
let expiryTimer = 0;

function render(v: View) {
  view = v;
  clearInterval(expiryTimer);
  app.replaceChildren(...build(v));
}

function build(v: View): Node[] {
  switch (v.state) {
    case "idle":
      return [h("div", { class: "centre" },
        h("h1", {}, "Waiting for AAB"),
        h("p", { class: "muted" }, "Start a job from AAB and it will open here."))];
    case "loading":
      return [h("div", { class: "centre" },
        h("p", { class: "muted" }, "Reading the request…"),
        h("div", { class: "meter busy loading" }, h("div")))];
    case "failed":
      return [h("div", { class: "centre" },
        h("h1", {}, v.message === "Cancelled." ? "Cancelled" : "Something went wrong"),
        v.message !== "Cancelled." && h("p", { class: "error" }, v.message),
        h("button", { onclick: () => call("dismiss") }, "Close"))];
    case "ready":
      return buildReady(v.manifest, v.source_high_bit, v.source_alpha);
    case "running":
      return buildRunning(v.title, v.stage);
    case "partial":
      return [
        h("h1", {}, v.title),
        h("div", { class: "card" },
          h("p", { class: v.message === "Cancelled." ? "" : "error" }, v.message),
          h("p", {}, `${v.written} frame${v.written === 1 ? " was" : "s were"} already written to:`),
          h("p", { class: "small muted" }, v.frames_dir)),
        h("div", { class: "actions" },
          h("button", { class: "danger", onclick: () => call("resolve_partial", { keep: false }) }, "Delete frames"),
          h("button", { class: "primary", onclick: () => call("resolve_partial", { keep: true }) }, "Keep frames")),
      ];
    case "done":
      return [
        h("h1", {}, v.title || "Done"),
        h("div", { class: "card" },
          h("p", {}, `${v.frames.toLocaleString()} frames written.`),
          v.warning && h("p", { class: "warning" }, v.warning),
          h("dl", { class: "facts" },
            h("dt", {}, "Frames"), h("dd", {}, v.frames_dir),
            v.video && h("dt", {}, "Video"), v.video && h("dd", {}, v.video))),
        h("div", { class: "actions" },
          h("button", { onclick: () => call("dismiss") }, "Done"),
          h("button", { class: "primary", onclick: () => call("open_output") }, "Open folder")),
      ];
  }
}

// ---- ready: collect the user's choices

function buildReady(m: Manifest, highBit: boolean, alpha: boolean): Node[] {
  const opts: StartOptions = { frames_parent: "", keep_video: false, video_dir: null, depth: "eight" };
  const p = m.params;

  const expiry = h("dd", {});
  const updateExpiry = () => {
    if (!m.input_expires_at) { expiry.textContent = "—"; return; }
    const left = m.input_expires_at - Date.now() / 1000;
    expiry.textContent = left > 0 ? `in ${duration(left)}` : "expired — start again from AAB";
    expiry.className = left > 0 ? (left < 600 ? "warning" : "") : "error";
  };
  updateExpiry();
  expiryTimer = window.setInterval(updateExpiry, 1000);

  const facts = h("dl", { class: "facts" },
    h("dt", {}, "File"), h("dd", {}, m.input.filename),
    h("dt", {}, "Size"), h("dd", {}, bytes(m.input.size)),
    h("dt", {}, "Frames"), h("dd", {}, `${p.frames.toLocaleString()} · ${p.width}×${p.height}${p.fps ? ` · ${p.fps} fps` : ""}`),
    p.pix_fmt && h("dt", {}, "Format"), p.pix_fmt && h("dd", {}, p.pix_fmt),
    h("dt", {}, "Link expires"), expiry,
    m.job_id && h("dt", {}, "Job"), m.job_id && h("dd", { class: "muted" }, m.job_id));

  const framesPath = h("div", { class: "path empty", title: "" }, "No folder chosen");
  const videoPath = h("div", { class: "path empty" }, "Same as frames folder");
  const setPath = (el: HTMLElement, path: string | null, empty: string) => {
    el.textContent = path ? `\u200E${path}\u200E` : empty;
    el.title = path ?? "";
    el.classList.toggle("empty", !path);
  };

  const keep = h("input", { type: "checkbox", id: "keep" });
  const videoField = h("div", { class: "field", hidden: true },
    h("label", {}, "Save the video in"),
    h("div", { class: "picker" }, videoPath,
      h("button", { onclick: async () => {
        const d = await call<string | null>("pick_folder", { title: "Where should the video be saved?" });
        if (d) { opts.video_dir = d; setPath(videoPath, d, ""); refresh(); }
      } }, "Choose…")));
  keep.onchange = () => {
    opts.keep_video = keep.checked;
    videoField.hidden = !keep.checked;
    if (keep.checked && !opts.video_dir && opts.frames_parent) {
      opts.video_dir = opts.frames_parent;
      setPath(videoPath, opts.video_dir, "");
    }
    refresh();
  };

  const alphaNote = alpha ? " + alpha" : "";
  const depth = h("select", {},
    h("option", { value: "eight" }, `8-bit${alphaNote}`),
    h("option", { value: "sixteen" }, `16-bit${alphaNote}`));
  const depthHint = highBit
    ? h("p", { class: "small warning" }, `This video is ${p.pix_fmt ?? "high bit depth"}; choose 16-bit to keep its full precision.`)
    : null;
  depth.onchange = () => { opts.depth = depth.value as Depth; refresh(); };

  const disk = h("div", { class: "disk", hidden: true });
  const start = h("button", { class: "primary", disabled: true }, "Start");
  let seq = 0;
  async function refresh() {
    const mine = ++seq;
    start.disabled = true;
    if (!opts.frames_parent || (opts.keep_video && !opts.video_dir)) { disk.hidden = true; return; }
    const c = await call<DiskCheck>("check_disk", { opts });
    if (mine !== seq || !c) return;
    disk.hidden = false;
    disk.className = `disk ${c.verdict}`;
    const need = `Needs about ${bytes(c.frames_low + c.video)}–${bytes(c.frames_high + c.video)}`;
    const free = c.volumes.map((v) => bytes(v.free)).join(" / ");
    disk.textContent =
      c.verdict === "block" ? `${need}, only ${free} free. Choose another folder.`
      : c.verdict === "warn" ? `${need}, ${free} free. It may run out of space.`
      : `${need}, ${free} free.`;
    start.disabled = c.verdict === "block";
  }

  start.onclick = async () => {
    start.disabled = true;
    await call("start", { opts });
  };

  return [
    h("h1", {}, m.title),
    h("div", { class: "card" }, facts),
    h("div", { class: "card" },
      h("div", { class: "field" },
        h("label", {}, "Save the frames in"),
        h("div", { class: "picker" }, framesPath,
          h("button", { onclick: async () => {
            const d = await call<string | null>("pick_folder", { title: "Where should the frames go?" });
            if (d) {
              opts.frames_parent = d;
              setPath(framesPath, d, "");
              if (opts.keep_video && !opts.video_dir) { opts.video_dir = d; setPath(videoPath, d, ""); }
              refresh();
            }
          } }, "Choose…")),
        h("p", { class: "small muted" }, "A new folder named after the file is created inside it.")),
      h("label", { class: "check" }, keep, "Keep the downloaded video"),
      videoField,
      h("div", { class: "field" }, h("label", {}, "PNG bit depth"), depth, depthHint),
      disk),
    h("div", { class: "actions" },
      h("button", { onclick: () => call("dismiss") }, "Cancel"),
      start),
  ];
}

// ---- running: progress for the current stage

const STAGES: [Stage, string][] = [
  ["download", "Download"], ["probe", "Check"], ["decode", "Decode"], ["cleanup", "Finish"],
];
let meter: HTMLDivElement | null = null;
let stats: HTMLDivElement | null = null;
let rate = { stage: "" as string, t0: 0, d0: 0 };

function buildRunning(title: string, stage: Stage): Node[] {
  const idx = STAGES.findIndex(([s]) => s === stage);
  meter = h("div");
  stats = h("div", { class: "stats" }, h("span", {}, "Starting…"), h("span", {}));
  rate = { stage, t0: performance.now(), d0: 0 };
  const busy = stage === "probe" || stage === "cleanup";
  return [
    h("h1", {}, title),
    h("div", { class: "card" },
      h("div", { class: "steps" }, ...STAGES.map(([, label], i) =>
        h("span", { class: i < idx ? "done" : i === idx ? "on" : "" }, `${i ? "› " : ""}${label}`))),
      h("div", { class: busy ? "meter busy" : "meter" }, meter),
      stats),
    h("div", { class: "actions" },
      h("button", { class: "danger", onclick: () => call("cancel") }, "Cancel")),
  ];
}

function onProgress(p: Progress) {
  if (view.state !== "running" || view.stage !== p.stage || !meter || !stats) return;
  const frac = p.total ? Math.min(1, p.done / p.total) : 0;
  meter.style.width = `${(frac * 100).toFixed(1)}%`;
  const secs = (performance.now() - rate.t0) / 1000;
  const speed = secs > 0.5 ? (p.done - rate.d0) / secs : 0;
  const eta = speed > 0 ? (p.total - p.done) / speed : NaN;
  const [left, right] = stats.children as unknown as HTMLElement[];
  if (p.stage === "download") {
    left.textContent = `${bytes(p.done)} of ${bytes(p.total)}${speed ? ` · ${bytes(speed)}/s` : ""}`;
  } else {
    left.textContent = `Frame ${p.done.toLocaleString()} of ${p.total.toLocaleString()}${speed ? ` · ${speed.toFixed(1)} fps` : ""}`;
  }
  right.textContent = isNaN(eta) ? "" : `${duration(eta)} left`;
}

// ---------------------------------------------------------------- wiring

listen<View>("view", (e) => render(e.payload));
listen<Progress>("progress", (e) => onProgress(e.payload));
listen<string>("notice", (e) => toast(e.payload));
call<View>("get_view").then((v) => v && render(v));
