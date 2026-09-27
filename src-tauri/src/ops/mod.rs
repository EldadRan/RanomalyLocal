//! The tools the helper can run, one module each, behind one small contract.
//!
//! ## Adding an op
//!
//! 1. Write `ops/<name>.rs` with a `Job` that has `from_manifest`, `default_title`, `details`,
//!    `preflight`, `start_check` and `run` (copy the shape of `video_to_png`).
//! 2. Add a variant to [`Job`] and one line to each `match` below — the compiler lists them.
//! 3. Add `src/ops/<name>.ts` for its setup screen and register it in `src/ops/index.ts`.
//! 4. Document its manifest fields in docs/manifest.md.
//!
//! Everything else — links, allowlist, state, the running/done/partial/failed screens,
//! progress, Cancel, sleep prevention, one-job-at-a-time, quitting — is shared.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::manifest::{Envelope, ManifestError};

pub mod video_to_png;

/// A validated job, ready for the user's choices.
pub enum Job {
    VideoToPng(video_to_png::Job),
}

/// Picks the op named by the envelope and lets it parse the rest of the document.
pub fn prepare(env: &Envelope, doc: Value) -> Result<Job, ManifestError> {
    match env.op.as_str() {
        video_to_png::OP => Ok(Job::VideoToPng(video_to_png::Job::from_manifest(doc)?)),
        other => Err(ManifestError::Unsupported(format!("tool \"{other}\""))),
    }
}

impl Job {
    pub fn op(&self) -> &'static str {
        match self {
            Job::VideoToPng(_) => video_to_png::OP,
        }
    }

    /// Stage names shown while running, in order. `Ctx::stage` takes an index into this.
    pub fn stages(&self) -> &'static [&'static str] {
        match self {
            Job::VideoToPng(_) => video_to_png::STAGES,
        }
    }

    /// Title when the manifest has none.
    pub fn default_title(&self) -> String {
        match self {
            Job::VideoToPng(j) => j.default_title(),
        }
    }

    /// Whatever the op's setup screen needs, as JSON.
    pub fn details(&self) -> Value {
        match self {
            Job::VideoToPng(j) => j.details(),
        }
    }

    /// Live feedback on the user's choices before starting (e.g. disk space).
    pub fn preflight(&self, opts: &Value) -> Result<Value, String> {
        match self {
            Job::VideoToPng(j) => j.preflight(opts),
        }
    }

    /// Refuses a start the op cannot run; the job has not begun when this errs.
    pub fn start_check(&self, opts: &Value) -> Result<(), String> {
        match self {
            Job::VideoToPng(j) => j.start_check(opts),
        }
    }

    pub async fn run(&self, ctx: &Ctx, opts: Value) -> Outcome {
        match self {
            Job::VideoToPng(j) => j.run(ctx, opts).await,
        }
    }
}

// ---------------------------------------------------------------- what an op reports

/// A labelled value on the done / partial screens.
#[derive(Debug, Clone, Serialize)]
pub struct Fact {
    pub label: String,
    pub value: String,
    /// Numbers, codes and sizes are set in mono.
    pub mono: bool,
}

impl Fact {
    pub fn text(label: &str, value: impl Into<String>) -> Self {
        Fact { label: label.into(), value: value.into(), mono: false }
    }
    pub fn mono(label: &str, value: impl Into<String>) -> Self {
        Fact { label: label.into(), value: value.into(), mono: true }
    }
}

/// Output the user may delete after a cancel or failure. Only what the op wrote.
#[derive(Debug, Clone, Default)]
pub struct Discard {
    pub files: Vec<PathBuf>,
    /// Removed after the files, and only if empty.
    pub dirs: Vec<PathBuf>,
}

pub enum Outcome {
    Done {
        facts: Vec<Fact>,
        warning: Option<String>,
        /// What "Open folder" opens.
        open: Option<PathBuf>,
    },
    /// Cancelled or failed after writing something: the user keeps it or deletes it.
    Partial {
        message: String,
        facts: Vec<Fact>,
        discard: Discard,
        open: Option<PathBuf>,
    },
    Failed {
        message: String,
    },
}

pub const CANCELLED: &str = "Cancelled.";

// ---------------------------------------------------------------- what an op is given

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Bytes,
    Frames,
    #[allow(dead_code)] // for ops that count things other than bytes or frames
    Items,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub stage: usize,
    pub done: u64,
    pub total: u64,
    pub unit: Unit,
}

/// How a running op talks to the shell. The app implements it by updating the window;
/// tests implement it by recording. Ops depend on this, never on Tauri.
pub trait Events: Send + Sync {
    /// The running screen moves to stage `index` of `Job::stages`.
    fn stage(&self, index: usize);
    fn progress(&self, p: Progress);
}

/// The running job's handle on the shell: cancellation, stage changes and progress.
pub struct Ctx {
    pub cancel: CancellationToken,
    pub events: Arc<dyn Events>,
}

impl Ctx {
    pub fn stage(&self, index: usize) {
        self.events.stage(index);
    }

    /// A progress reporter for `stage`, throttled to five updates a second.
    pub fn reporter(&self, stage: usize, unit: Unit) -> Reporter {
        Reporter { events: self.events.clone(), stage, unit, last: None }
    }
}

pub struct Reporter {
    events: Arc<dyn Events>,
    stage: usize,
    unit: Unit,
    last: Option<Instant>,
}

impl Reporter {
    pub fn report(&mut self, done: u64, total: u64) {
        if self.last.is_some_and(|t| t.elapsed() < Duration::from_millis(200)) && done < total {
            return;
        }
        self.last = Some(Instant::now());
        self.events.progress(Progress { stage: self.stage, done, total, unit: self.unit });
    }
}
