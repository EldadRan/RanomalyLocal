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
let toastHover = false;
toastEl.onmouseenter = () => { toastHover = true; clearTimeout(toastTimer); };
toastEl.onmouseleave = () => { toastHover = false; hideToastLater(); };
function hideToastLater() {
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => { if (!toastHover) toastEl.classList.remove("show"); }, 6000);
}
function toast(msg: string) {
  toastEl.textContent = cap(msg);
  toastEl.classList.remove("show");
  void toastEl.offsetWidth;
  toastEl.classList.add("show");
  hideToastLater();
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    toast(String(e));
    return undefined;
  }
}

const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
const label = (text: string) => h("span", { class: "label" }, text);

// ---------------------------------------------------------------- views

let view: View = { state: "idle" };
let expiryTimer = 0;

function render(v: View) {
  view = v;
  clearInterval(expiryTimer);
  app.replaceChildren(...build(v).filter((n): n is Node => !!n));
}

function build(v: View): (Node | false | null | undefined | "")[] {
  switch (v.state) {
    case "idle":
      return [h("div", { class: "centre" },
        h("h1", {}, "Waiting for AAB"),
        h("p", { class: "muted small" }, "Start a job from AAB and it opens here."))];
    case "loading":
      return [h("div", { class: "centre" }, h("p", { class: "muted small" }, "Reading the request…"))];
    case "failed": {
      const cancelled = v.message === "Cancelled.";
      return [
        h("h1", {}, cancelled ? "Cancelled" : "Could not run this job"),
        !cancelled && h("div", { class: "card" }, h("p", { class: "error small" }, cap(v.message))),
        h("div", { class: "actions" },
          h("button", { class: "secondary", onclick: () => call("dismiss") }, "Close")),
      ];
    }
    case "ready":
      return buildReady(v.manifest, v.source_alpha);
    case "running":
      return buildRunning(v.title, v.stage);
    case "partial":
      return [
        h("h1", {}, v.title),
        h("div", { class: "card" },
          v.message !== "Cancelled." && h("p", { class: "error small" }, cap(v.message)),
          h("dl", { class: "facts" },
            h("dt", {}, "Written"), h("dd", { class: "num" }, `${v.written.toLocaleString()} frames`),
            h("dt", {}, "Folder"), h("dd", {}, v.frames_dir))),
        h("div", { class: "actions" },
          h("button", { class: "secondary danger", onclick: () => call("resolve_partial", { keep: false }) }, "Delete frames"),
          h("button", { class: "primary", onclick: () => call("resolve_partial", { keep: true }) }, "Keep frames")),
      ];
    case "done":
      return [
        h("h1", {}, v.title || "Done"),
        h("div", { class: "card" },
          h("dl", { class: "facts" },
            h("dt", {}, "Frames"), h("dd", { class: "num" }, v.frames.toLocaleString()),
            h("dt", {}, "Folder"), h("dd", {}, v.frames_dir),
            v.video && h("dt", {}, "Video"), v.video && h("dd", {}, v.video)),
          v.warning && h("p", { class: "small" }, v.warning)),
        h("div", { class: "actions" },
          h("button", { class: "secondary", onclick: () => call("dismiss") }, "Done"),
          h("button", { class: "primary", onclick: () => call("open_output") }, "Open folder")),
      ];
  }
}

// ---- ready: collect the user's choices

function buildReady(m: Manifest, alpha: boolean): Node[] {
  const opts: StartOptions = { frames_parent: "", keep_video: false, video_dir: null, depth: "eight" };
  const p = m.params;

  const expiry = h("dd", { class: "num" });
  const updateExpiry = () => {
    if (!m.input_expires_at) { expiry.textContent = "—"; return; }
    const left = m.input_expires_at - Date.now() / 1000;
    expiry.textContent = left > 0 ? `in ${duration(left)}` : "expired — start again from AAB";
    expiry.className = left > 0 ? (left < 600 ? "num warn" : "num") : "error";
  };
  updateExpiry();
  expiryTimer = window.setInterval(updateExpiry, 1000);

  const facts = h("dl", { class: "facts" },
    h("dt", {}, "File"), h("dd", {}, m.input.filename),
    h("dt", {}, "Size"), h("dd", { class: "num" }, bytes(m.input.size)),
    h("dt", {}, "Frames"), h("dd", { class: "num" }, `${p.frames.toLocaleString()} · ${p.width}×${p.height}${p.fps ? ` · ${p.fps} fps` : ""}`),
    p.pix_fmt && h("dt", {}, "Format"), p.pix_fmt && h("dd", { class: "num" }, p.pix_fmt),
    h("dt", {}, "Link"), expiry,
    m.job_id && h("dt", {}, "Job"), m.job_id && h("dd", { class: "num muted" }, m.job_id));

  const framesPath = h("div", { class: "path empty" }, "No folder chosen");
  const videoPath = h("div", { class: "path empty" }, "No folder chosen");
  const setPath = (el: HTMLElement, path: string | null) => {
    el.textContent = path ? `\u200E${path}\u200E` : "No folder chosen";
    el.title = path ?? "";
    el.classList.toggle("empty", !path);
  };
  const choose = (title: string, set: (d: string) => void) =>
    h("button", { onclick: async () => {
      const d = await call<string | null>("pick_folder", { title });
      if (d) { set(d); refresh(); }
    } }, "Choose…");

  const keep = h("input", { type: "checkbox", class: "toggle", id: "keep" });
  const videoField = h("div", { class: "field", hidden: true },
    label("Video folder"),
    h("div", { class: "picker" }, videoPath,
      choose("Where should the video be saved?", (d) => { opts.video_dir = d; setPath(videoPath, d); })));
  keep.onchange = () => {
    opts.keep_video = keep.checked;
    videoField.hidden = !keep.checked;
    if (keep.checked && !opts.video_dir && opts.frames_parent) {
      opts.video_dir = opts.frames_parent;
      setPath(videoPath, opts.video_dir);
    }
    refresh();
  };

  const suffix = alpha ? " + ALPHA" : "";
  const depthButtons = (["eight", "sixteen"] as Depth[]).map((d) =>
    h("button", { type: "button", ariaPressed: String(d === opts.depth), onclick: () => {
      opts.depth = d;
      depthButtons.forEach((b, i) => b.ariaPressed = String(["eight", "sixteen"][i] === d));
      refresh();
    } }, `${d === "eight" ? "8-BIT" : "16-BIT"}${suffix}`));

  const disk = h("p", { class: "disk", hidden: true });
  const start = h("button", { class: "primary", hidden: true, onclick: async () => {
    start.hidden = true;
    await call("start", { opts });
  } }, "Start");

  // Start is absent, not disabled, until the choices can actually run (design system §6).
  let seq = 0;
  async function refresh() {
    const mine = ++seq;
    start.hidden = true;
    if (!opts.frames_parent || (opts.keep_video && !opts.video_dir)) { disk.hidden = true; return; }
    const c = await call<DiskCheck>("check_disk", { opts });
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
    h("h1", {}, m.title),
    h("div", { class: "card" }, facts),
    h("div", { class: "card" },
      h("div", { class: "field" },
        label("Frames folder"),
        h("div", { class: "picker" }, framesPath,
          choose("Where should the frames go?", (d) => {
            opts.frames_parent = d;
            setPath(framesPath, d);
            if (opts.keep_video && !opts.video_dir) { opts.video_dir = d; setPath(videoPath, d); }
          }))),
      h("label", { class: "row", htmlFor: "keep" }, h("span", { class: "small dim" }, "Keep the downloaded video"), keep),
      videoField,
      h("div", { class: "row" }, label("PNG bit depth"), h("div", { class: "seg" }, ...depthButtons)),
      disk),
    h("div", { class: "actions" },
      h("button", { class: "secondary", onclick: () => call("dismiss") }, "Cancel"),
      start),
  ];
}

// ---- running: progress for the current stage

const STAGES: [Stage, string][] = [
  ["download", "Download"], ["probe", "Check"], ["decode", "Decode"], ["cleanup", "Finish"],
];
let meter: HTMLDivElement | null = null;
let stats: HTMLDivElement | null = null;
let rate = { t0: 0, d0: 0 };

function buildRunning(title: string, stage: Stage): Node[] {
  const idx = STAGES.findIndex(([s]) => s === stage);
  meter = h("div");
  const quick = stage === "probe" || stage === "cleanup";
  stats = h("div", { class: "stats" }, h("span", {}, quick ? "Working…" : "Starting…"), h("span", {}));
  rate = { t0: performance.now(), d0: 0 };
  return [
    h("h1", {}, title),
    h("div", { class: "card" },
      h("div", { class: "steps" }, ...STAGES.map(([, name], i) =>
        h("span", { class: i < idx ? "done" : i === idx ? "on" : "" }, `${i ? "› " : ""}${name}`))),
      !quick && h("div", { class: "meter" }, meter),
      stats),
    h("div", { class: "actions" },
      h("button", { class: "secondary danger", onclick: () => call("cancel") }, "Cancel")),
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
  left.textContent = p.stage === "download"
    ? `${bytes(p.done)} / ${bytes(p.total)}${speed ? ` · ${bytes(speed)}/s` : ""}`
    : `${p.done.toLocaleString()} / ${p.total.toLocaleString()} frames${speed ? ` · ${speed.toFixed(1)} fps` : ""}`;
  right.textContent = isNaN(eta) ? "" : `${duration(eta)} left`;
}

// ---------------------------------------------------------------- wiring

// The window follows the content: report the gap between what the content needs and what
// the webview shows, and Rust grows or shrinks the window by exactly that.
function fit() {
  const need = Math.ceil(app.getBoundingClientRect().height);
  const gap = need - window.innerHeight;
  if (gap !== 0) invoke("fit_window", { delta: gap }).catch(() => {});
}
new ResizeObserver(fit).observe(app);
window.addEventListener("resize", fit);

listen<View>("view", (e) => render(e.payload));
listen<Progress>("progress", (e) => onProgress(e.payload));
listen<string>("notice", (e) => toast(e.payload));
call<View>("get_view").then((v) => v && render(v));
