//! The shell every op runs in: link → manifest → setup → running → outcome → quit.
//!
//! The window mirrors `View`; every change is pushed as a `view` event, and progress within a
//! stage as `progress` events (see `ops::Reporter`). Ops never touch this state directly.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

use crate::link;
use crate::manifest::{self, Envelope};
use crate::ops::{self, Ctx, Discard, Events, Fact, Job, Outcome, Progress};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum View {
    Idle,
    Loading,
    /// The op's setup screen: `details` is whatever that op's UI needs.
    Ready {
        op: String,
        /// Host the manifest came from, shown so the user sees where a job originates.
        source: String,
        title: String,
        job_id: Option<String>,
        details: Value,
    },
    Running {
        title: String,
        stages: Vec<String>,
        stage: usize,
    },
    /// Cancelled or failed after output was written: keep it or delete it?
    Partial {
        title: String,
        message: String,
        facts: Vec<Fact>,
    },
    Done {
        title: String,
        facts: Vec<Fact>,
        warning: Option<String>,
        can_open: bool,
    },
    Failed {
        message: String,
    },
}

#[derive(Default)]
pub struct AppState {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    view: Option<View>,
    job: Option<Arc<Job>>,
    title: String,
    cancel: Option<CancellationToken>,
    /// Bumped on every new link so a slow manifest fetch cannot overwrite a newer one.
    generation: u64,
    /// Kept in Rust so the webview can never ask to open or delete an arbitrary path.
    open: Option<PathBuf>,
    discard: Option<Discard>,
}

impl AppState {
    pub fn view(&self) -> View {
        self.inner.lock().unwrap().view.clone().unwrap_or(View::Idle)
    }

    fn is_busy(&self) -> bool {
        matches!(self.view(), View::Running { .. } | View::Partial { .. })
    }
}

fn set_view(app: &AppHandle, view: View) {
    app.state::<AppState>().inner.lock().unwrap().view = Some(view.clone());
    let _ = app.emit("view", view);
}

/// What a running op's `Ctx` drives: the window.
struct WindowEvents(AppHandle);

impl Events for WindowEvents {
    fn stage(&self, stage: usize) {
        if let View::Running { title, stages, .. } = self.0.state::<AppState>().view() {
            set_view(&self.0, View::Running { title, stages, stage });
        }
    }

    fn progress(&self, p: Progress) {
        let _ = self.0.emit("progress", p);
    }
}

pub fn notice(app: &AppHandle, message: &str) {
    let _ = app.emit("notice", message);
}

pub fn focus_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        // Up to five hops, and never from https down to plain http.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if link::check_url(attempt.url()).is_err() {
                attempt.error("redirected to a non-https address")
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("Ranomaly-Local/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

// ---------------------------------------------------------------- links

pub fn handle_link(app: &AppHandle, raw: &str) {
    focus_window(app);
    let state = app.state::<AppState>();
    if state.is_busy() {
        notice(app, "A job is already running. Finish or cancel it before starting another.");
        return;
    }
    let url = match link::parse_link(raw) {
        Ok(u) => u,
        Err(e) => return set_view(app, View::Failed { message: e.to_string() }),
    };
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        inner.generation += 1;
        inner.job = None;
        inner.generation
    };
    set_view(app, View::Loading);

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = manifest::fetch(&http_client(), &url)
            .await
            .and_then(|(env, doc)| ops::prepare(&env, doc).map(|job| (env, job)));
        let state = app.state::<AppState>();
        if state.inner.lock().unwrap().generation != generation {
            return;
        }
        match result {
            Ok((env, job)) => ready(&app, env, job, url.host_str().unwrap_or_default().to_string()),
            Err(e) => set_view(&app, View::Failed { message: e.to_string() }),
        }
    });
}

fn ready(app: &AppHandle, env: Envelope, job: Job, source: String) {
    let title = manifest::display_text(env.title, 200).unwrap_or_else(|| job.default_title());
    let view = View::Ready {
        op: job.op().to_string(),
        source,
        title: title.clone(),
        job_id: manifest::display_text(env.job_id, 100),
        details: job.details(),
    };
    {
        let state = app.state::<AppState>();
        let mut inner = state.inner.lock().unwrap();
        inner.job = Some(Arc::new(job));
        inner.title = title;
    }
    set_view(app, view);
}

// ---------------------------------------------------------------- commands

fn ready_job(app: &AppHandle) -> Result<(Arc<Job>, String), String> {
    let state = app.state::<AppState>();
    let inner = state.inner.lock().unwrap();
    match (&inner.view, &inner.job) {
        (Some(View::Ready { .. }), Some(job)) => Ok((job.clone(), inner.title.clone())),
        _ => Err("there is no request waiting to start".into()),
    }
}

pub fn preflight(app: &AppHandle, opts: &Value) -> Result<Value, String> {
    ready_job(app)?.0.preflight(opts)
}

pub fn start(app: &AppHandle, opts: Value) -> Result<(), String> {
    let (job, title) = ready_job(app)?;
    job.start_check(&opts)?;
    let cancel = CancellationToken::new();
    app.state::<AppState>().inner.lock().unwrap().cancel = Some(cancel.clone());
    let stages = job.stages().iter().map(|s| s.to_string()).collect();
    set_view(app, View::Running { title: title.clone(), stages, stage: 0 });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _awake = keepawake::Builder::default()
            .idle(true)
            .reason("Running a Ranomaly job")
            .app_name("Ranomaly Local")
            .app_reverse_domain("com.ranomaly.ext")
            .create()
            .ok();
        let ctx = Ctx { cancel, events: Arc::new(WindowEvents(app.clone())) };
        let outcome = job.run(&ctx, opts).await;
        finish(&app, title, outcome);
    });
    Ok(())
}

fn finish(app: &AppHandle, title: String, outcome: Outcome) {
    let state = app.state::<AppState>();
    let view = {
        let mut inner = state.inner.lock().unwrap();
        inner.cancel = None;
        match outcome {
            Outcome::Done { facts, warning, open } => {
                inner.open = open;
                View::Done { title, facts, warning, can_open: inner.open.is_some() }
            }
            Outcome::Partial { message, facts, discard, open } => {
                inner.open = open;
                inner.discard = Some(discard);
                View::Partial { title, message, facts }
            }
            Outcome::Failed { message } => View::Failed { message },
        }
    };
    set_view(app, view);
}

pub fn cancel(app: &AppHandle) {
    if let Some(c) = &app.state::<AppState>().inner.lock().unwrap().cancel {
        c.cancel();
    }
}

/// Keep: the partial output becomes the outcome. Delete: remove what the op wrote, then quit.
pub async fn resolve_partial(app: &AppHandle, keep: bool) -> Result<(), String> {
    let View::Partial { title, facts, .. } = app.state::<AppState>().view() else {
        return Err("nothing to resolve".into());
    };
    let discard = app.state::<AppState>().inner.lock().unwrap().discard.take().unwrap_or_default();
    if keep {
        let can_open = app.state::<AppState>().inner.lock().unwrap().open.is_some();
        set_view(app, View::Done { title, facts, warning: Some("Partial output kept.".into()), can_open });
        return Ok(());
    }
    tokio::task::spawn_blocking(move || remove(&discard))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("could not delete the output: {e}"))?;
    app.exit(0);
    Ok(())
}

fn remove(d: &Discard) -> std::io::Result<()> {
    for f in &d.files {
        match std::fs::remove_file(f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    for dir in &d.dirs {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(())
}

/// The app exists for one job: once the user closes the outcome (Done, Close, or Cancel before
/// starting) it quits. The calling app starts it again with the next link.
pub fn dismiss(app: &AppHandle) {
    if !app.state::<AppState>().is_busy() {
        app.exit(0);
    }
}

pub fn output_path(app: &AppHandle) -> Option<PathBuf> {
    let state = app.state::<AppState>();
    let inner = state.inner.lock().unwrap();
    matches!(inner.view, Some(View::Done { .. })).then(|| inner.open.clone()).flatten()
}

pub fn is_running(app: &AppHandle) -> bool {
    matches!(app.state::<AppState>().view(), View::Running { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_listed_files_and_empty_dirs() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("out");
        std::fs::create_dir(&f).unwrap();
        std::fs::write(f.join("a.png"), b"").unwrap();
        std::fs::write(f.join("notes.txt"), b"").unwrap();
        let discard = Discard { files: vec![f.join("a.png"), f.join("gone.png")], dirs: vec![f.clone()] };
        remove(&discard).unwrap();
        assert!(!f.join("a.png").exists());
        assert!(f.join("notes.txt").exists(), "unlisted files stay, so the dir stays");
    }
}
