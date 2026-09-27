// The shell: every screen except an op's setup screen (those live in src/ops/).
// Mirrors `View` in src-tauri/src/job.rs.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ops } from "./ops";
import { bytes, call, cap, duration, facts, h, toast, type Fact } from "./ui";

type Unit = "bytes" | "frames" | "items";

type View =
  | { state: "idle" }
  | { state: "loading" }
  | { state: "ready"; op: string; source: string; title: string; job_id: string | null; details: unknown }
  | { state: "running"; title: string; stages: string[]; stage: number }
  | { state: "partial"; title: string; message: string; facts: Fact[] }
  | { state: "done"; title: string; facts: Fact[]; warning: string | null; can_open: boolean }
  | { state: "failed"; message: string };

interface Progress { stage: number; done: number; total: number; unit: Unit }

const CANCELLED = "Cancelled.";
const app = document.getElementById("app")!;

let view: View = { state: "idle" };
let leave: (() => void)[] = [];

function render(v: View) {
  // A stage change re-renders only the step line, so the progress bar keeps its place.
  if (v.state === "running" && view.state === "running" && running) {
    view = v;
    running.setStage(v.stage);
    return;
  }
  leave.forEach((fn) => fn());
  leave = [];
  view = v;
  running = null;
  app.replaceChildren(...build(v).filter((n): n is Node => !!n));
}

function build(v: View): (Node | false | null | undefined)[] {
  switch (v.state) {
    case "idle":
      return [h("div", { class: "centre" },
        h("h1", {}, "Waiting for AAB"),
        h("p", { class: "muted small" }, "Start a job from AAB and it opens here."))];
    case "loading":
      return [h("div", { class: "centre" }, h("p", { class: "muted small" }, "Reading the request…"))];
    case "failed": {
      const cancelled = v.message === CANCELLED;
      return [
        h("h1", {}, cancelled ? "Cancelled" : "Could not run this job"),
        !cancelled && h("div", { class: "card" }, h("p", { class: "error small" }, cap(v.message))),
        h("div", { class: "actions" },
          h("button", { class: "secondary", onclick: () => call("dismiss") }, "Close")),
      ];
    }
    case "ready": {
      const ui = ops[v.op];
      if (!ui) return build({ state: "failed", message: `this version has no screen for "${v.op}"` });
      return ui.ready({ title: v.title, source: v.source, jobId: v.job_id, details: v.details, onLeave: (fn) => leave.push(fn) });
    }
    case "running":
      running = runningScreen(v);
      return running.nodes;
    case "partial":
      return [
        h("h1", {}, v.title),
        h("div", { class: "card" },
          v.message !== CANCELLED && h("p", { class: "error small" }, cap(v.message)),
          facts(v.facts)),
        h("div", { class: "actions" },
          h("button", { class: "secondary danger", onclick: () => call("resolve_partial", { keep: false }) }, "Delete output"),
          h("button", { class: "primary", onclick: () => call("resolve_partial", { keep: true }) }, "Keep output")),
      ];
    case "done":
      return [
        h("h1", {}, v.title),
        h("div", { class: "card" }, facts(v.facts), v.warning && h("p", { class: "small" }, v.warning)),
        h("div", { class: "actions" },
          h("button", { class: "secondary", onclick: () => call("dismiss") }, "Done"),
          v.can_open && h("button", { class: "primary", onclick: () => call("open_output") }, "Open folder")),
      ];
  }
}

// ---- running: the op's stages, with progress for the current one

let running: ReturnType<typeof runningScreen> | null = null;

function runningScreen(v: Extract<View, { state: "running" }>) {
  const steps = v.stages.map((name, i) => h("span", {}, `${i ? "› " : ""}${name}`));
  const bar = h("div");
  const meter = h("div", { class: "meter", hidden: true }, bar);
  const left = h("span", {}, "Working…");
  const right = h("span", {});
  let rate = { stage: -1, t0: 0, d0: 0 };

  function setStage(stage: number) {
    steps.forEach((s, i) => (s.className = i < stage ? "done" : i === stage ? "on" : ""));
    meter.hidden = true;
    left.textContent = "Working…";
    right.textContent = "";
  }
  setStage(v.stage);

  function progress(p: Progress) {
    if (p.stage !== (view as { stage?: number }).stage) return;
    if (rate.stage !== p.stage) rate = { stage: p.stage, t0: performance.now(), d0: p.done };
    meter.hidden = !p.total;
    bar.style.width = `${(p.total ? Math.min(1, p.done / p.total) * 100 : 0).toFixed(1)}%`;
    const secs = (performance.now() - rate.t0) / 1000;
    const speed = secs > 0.5 ? (p.done - rate.d0) / secs : 0;
    const eta = speed > 0 && p.total ? (p.total - p.done) / speed : NaN;
    const amount = (n: number) => (p.unit === "bytes" ? bytes(n) : n.toLocaleString());
    const per = p.unit === "bytes" ? `${bytes(speed)}/s` : p.unit === "frames" ? `${speed.toFixed(1)} fps` : `${speed.toFixed(1)}/s`;
    left.textContent = `${amount(p.done)}${p.total ? ` / ${amount(p.total)}` : ""}${p.unit === "frames" ? " frames" : ""}${speed ? ` · ${per}` : ""}`;
    right.textContent = isNaN(eta) ? "" : `${duration(eta)} left`;
  }

  const nodes = [
    h("h1", {}, v.title),
    h("div", { class: "card" }, h("div", { class: "steps" }, ...steps), meter, h("div", { class: "stats" }, left, right)),
    h("div", { class: "actions" },
      h("button", { class: "secondary danger", onclick: () => call("cancel") }, "Cancel")),
  ];
  return { nodes, setStage, progress };
}

// ---------------------------------------------------------------- wiring

// The window follows the content: report the gap between what the content needs and what
// the webview shows, and Rust grows or shrinks the window by exactly that.
function fit() {
  const gap = Math.ceil(app.getBoundingClientRect().height) - window.innerHeight;
  if (gap !== 0) invoke("fit_window", { delta: gap }).catch(() => {});
}
new ResizeObserver(fit).observe(app);
window.addEventListener("resize", fit);

listen<View>("view", (e) => render(e.payload));
listen<Progress>("progress", (e) => running?.progress(e.payload));
listen<string>("notice", (e) => toast(e.payload));
call<View>("get_view").then((v) => v && render(v));
