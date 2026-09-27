// Setup screens, one per op. The shell (src/main.ts) draws every other screen.
//
// Adding an op: write src/ops/<op>.ts exporting an OpUI, and add it to `ops` below under the
// same name the Rust side uses (`ops::<op>::OP`).

import { videoToPng } from "./video_to_png";

export interface ReadyContext {
  title: string;
  jobId: string | null;
  /** The op's own data from Rust (`Job::details`). */
  details: any;
  /** Register work to undo when the screen goes away (timers, listeners). */
  onLeave(fn: () => void): void;
}

export interface OpUI {
  /**
   * The setup screen: what the job is, the user's choices, and the actions. Start by calling
   * `invoke("start", { opts })`; live checks by `invoke("preflight", { opts })`. `opts` is the
   * op's own shape, parsed by its Rust module.
   */
  ready(ctx: ReadyContext): Node[];
}

export const ops: Record<string, OpUI> = {
  video_to_png: videoToPng,
};
