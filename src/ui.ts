// Shared building blocks for the shell and every op's setup screen.

import { invoke } from "@tauri-apps/api/core";

export interface Fact { label: string; value: string; mono: boolean }

/** Builds elements with textContent only — manifest strings never become HTML. */
export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> & { class?: string } = {},
  ...children: (Node | string | null | false | undefined)[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  const { class: cls, ...rest } = props;
  if (cls) el.className = cls;
  Object.assign(el, rest);
  for (const c of children) if (c !== null && c !== false && c !== undefined) el.append(c);
  return el;
}

export const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/** Mono uppercase label (design system: Label, 10 mono). */
export const label = (text: string) => h("span", { class: "label" }, text);

export function bytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < units.length - 1) { n /= 1000; i++; }
  return `${n.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

export function duration(s: number): string {
  if (!isFinite(s) || s < 0) return "—";
  s = Math.round(s);
  const hh = Math.floor(s / 3600), mm = Math.floor((s % 3600) / 60), ss = s % 60;
  return hh ? `${hh}h ${mm}m` : mm ? `${mm}m ${ss}s` : `${ss}s`;
}

/** A label/value grid. `rows` skips falsy entries so optional facts need no branching. */
export function facts(rows: (Fact | null | false | undefined | "")[]): HTMLDListElement {
  const dl = h("dl", { class: "facts" });
  for (const r of rows) {
    if (!r) continue;
    dl.append(h("dt", {}, r.label), h("dd", { class: r.mono ? "num" : "" }, r.value));
  }
  return dl;
}

export const fact = (label: string, value: string, mono = false): Fact => ({ label, value, mono });

// ---- toast (design system: six seconds, paused on hover)

const toastEl = document.getElementById("toast")!;
let toastTimer = 0;
let toastHover = false;
toastEl.onmouseenter = () => { toastHover = true; clearTimeout(toastTimer); };
toastEl.onmouseleave = () => { toastHover = false; hideLater(); };
function hideLater() {
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => { if (!toastHover) toastEl.classList.remove("show"); }, 6000);
}
export function toast(msg: string) {
  toastEl.textContent = cap(msg);
  toastEl.classList.remove("show");
  void toastEl.offsetWidth;
  toastEl.classList.add("show");
  hideLater();
}

/** Invokes a Rust command; errors become a toast and `undefined`. */
export async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    toast(String(e));
    return undefined;
  }
}

// ---- folder picker row

/** A path field plus "Choose…". Long paths keep their end visible. */
export function folderPicker(dialogTitle: string, onPick: (path: string) => void) {
  const path = h("div", { class: "path empty" }, "No folder chosen");
  const set = (p: string | null) => {
    path.textContent = p ? `‎${p}‎` : "No folder chosen";
    path.title = p ?? "";
    path.classList.toggle("empty", !p);
  };
  const button = h("button", { onclick: async () => {
    const d = await call<string | null>("pick_folder", { title: dialogTitle });
    if (d) { set(d); onPick(d); }
  } }, "Choose…");
  return { el: h("div", { class: "picker" }, path, button), set };
}

/** Selection-triad chips (design system §6). Returns the row; `onChange` gets the value. */
export function chips<T extends string>(options: [T, string][], initial: T, onChange: (v: T) => void) {
  const buttons = options.map(([value, text]) =>
    h("button", { type: "button", ariaPressed: String(value === initial), onclick: () => {
      buttons.forEach((b, i) => (b.ariaPressed = String(options[i][0] === value)));
      onChange(value);
    } }, text));
  return h("div", { class: "seg" }, ...buttons);
}

/** An AAB toggle switch; `id` lets a <label> wrap it. */
export function toggle(id: string, onChange: (on: boolean) => void) {
  const el = h("input", { type: "checkbox", class: "toggle", id });
  el.onchange = () => onChange(el.checked);
  return el;
}
