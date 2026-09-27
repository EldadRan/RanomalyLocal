//! App state machine and the `video_to_png` job.
//!
//! The window mirrors `View`; every change is pushed as a `view` event, and progress within
//! a stage as `progress` events.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

use crate::ffmpeg::{self, Depth};
use crate::manifest::{self, Manifest, Op};
use crate::{disk, download, link};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum View {
    Idle,
    Loading,
    Ready {
        manifest: Manifest,
        /// Best guess from the manifest's pix_fmt until the file is probed.
        source_high_bit: bool,
        source_alpha: bool,
    },
    Running {
        title: String,
        stage: Stage,
    },
    /// Cancelled or failed after frames were written: keep them or delete them?
    Partial {
        title: String,
        message: String,
        written: u64,
        frames_dir: String,
    },
    Done {
        title: String,
        frames_dir: String,
        frames: u64,
        expected: u64,
        video: Option<String>,
        warning: Option<String>,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Download,
    Probe,
    Decode,
    Cleanup,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub stage: Stage,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartOptions {
    pub frames_parent: String,
    pub keep_video: bool,
    pub video_dir: Option<String>,
    pub depth: Depth,
}

#[derive(Default)]
pub struct AppState {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    view: Option<View>,
    manifest: Option<Manifest>,
    cancel: Option<CancellationToken>,
    /// Bumped on every new link so a slow manifest fetch cannot overwrite a newer one.
    generation: u64,
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
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("AA-Ext/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

// ---------------------------------------------------------------- links

pub fn handle_link(app: &AppHandle, raw: &str) {
    focus_window(app);
    let state = app.state::<AppState>();
    if state.is_busy() {
        notice(app, "A job is already running. Finish or cancel it before starting another from AAB.");
        return;
    }
    let url = match link::parse_link(raw) {
        Ok(u) => u,
        Err(e) => return set_view(app, View::Failed { message: e.to_string() }),
    };
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        inner.generation += 1;
        inner.manifest = None;
        inner.generation
    };
    set_view(app, View::Loading);

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = manifest::fetch(&http_client(), &url).await;
        let state = app.state::<AppState>();
        if state.inner.lock().unwrap().generation != generation {
            return;
        }
        match result {
            Ok(m) => {
                let (source_high_bit, source_alpha) =
                    ffmpeg::guess_depth(m.params.pix_fmt.as_deref().unwrap_or(""));
                state.inner.lock().unwrap().manifest = Some(m.clone());
                set_view(&app, View::Ready { manifest: m, source_high_bit, source_alpha });
            }
            Err(e) => set_view(&app, View::Failed { message: e.to_string() }),
        }
    });
}

// ---------------------------------------------------------------- commands' helpers

fn ready_manifest(app: &AppHandle) -> Result<Manifest, String> {
    let state = app.state::<AppState>();
    let inner = state.inner.lock().unwrap();
    match (&inner.view, &inner.manifest) {
        (Some(View::Ready { .. }), Some(m)) => Ok(m.clone()),
        _ => Err("there is no request waiting to start".into()),
    }
}

fn resolve_dirs(opts: &StartOptions) -> Result<(PathBuf, PathBuf), String> {
    let frames_parent = PathBuf::from(&opts.frames_parent);
    if !frames_parent.is_dir() {
        return Err("choose a folder for the frames".into());
    }
    let video_dir = match (opts.keep_video, &opts.video_dir) {
        (true, Some(d)) if Path::new(d).is_dir() => PathBuf::from(d),
        (true, _) => return Err("choose a folder for the video".into()),
        (false, _) => frames_parent.clone(),
    };
    Ok((frames_parent, video_dir))
}

fn sixteen_bit(app: &AppHandle, depth: Depth) -> (bool, bool) {
    let (high, alpha) = match app.state::<AppState>().view() {
        View::Ready { source_high_bit, source_alpha, .. } => (source_high_bit, source_alpha),
        _ => (false, false),
    };
    let sixteen = match depth {
        Depth::Match => high,
        Depth::Eight => false,
        Depth::Sixteen => true,
    };
    (sixteen, alpha)
}

pub fn check_disk(app: &AppHandle, opts: &StartOptions) -> Result<disk::DiskCheck, String> {
    let m = ready_manifest(app)?;
    let (frames_parent, video_dir) = resolve_dirs(opts)?;
    let (sixteen, alpha) = sixteen_bit(app, opts.depth);
    disk::check(&m.params, m.input.size, &frames_parent, &video_dir, sixteen, alpha)
        .map_err(|e| format!("could not read free space: {e}"))
}

pub fn start(app: &AppHandle, opts: StartOptions) -> Result<(), String> {
    let m = ready_manifest(app)?;
    let check = check_disk(app, &opts)?;
    if check.verdict == disk::Verdict::Block {
        return Err("there is not enough free disk space for this job".into());
    }
    let (frames_parent, video_dir) = resolve_dirs(&opts)?;
    let cancel = CancellationToken::new();
    app.state::<AppState>().inner.lock().unwrap().cancel = Some(cancel.clone());
    set_view(app, View::Running { title: m.title.clone(), stage: Stage::Download });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _awake = keepawake::Builder::default()
            .idle(true)
            .reason("Decoding video frames")
            .app_name("Ranomaly Ext")
            .app_reverse_domain("com.ranomany.aaext")
            .create()
            .ok();
        let end = match m.op {
            Op::VideoToPng => video_to_png(&app, &m, &opts, frames_parent, video_dir, &cancel).await,
        };
        app.state::<AppState>().inner.lock().unwrap().cancel = None;
        set_view(&app, end);
    });
    Ok(())
}

pub fn cancel(app: &AppHandle) {
    if let Some(c) = &app.state::<AppState>().inner.lock().unwrap().cancel {
        c.cancel();
    }
}

pub async fn resolve_partial(app: &AppHandle, keep: bool) -> Result<(), String> {
    let View::Partial { title, frames_dir, written, .. } = app.state::<AppState>().view() else {
        return Err("nothing to resolve".into());
    };
    if keep {
        set_view(app, View::Done {
            title,
            frames_dir,
            frames: written,
            expected: written,
            video: None,
            warning: Some("Partial output kept.".into()),
        });
        return Ok(());
    }
    let dir = PathBuf::from(&frames_dir);
    tokio::task::spawn_blocking(move || remove_frames(&dir))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("could not delete the frames: {e}"))?;
    set_view(app, View::Idle);
    Ok(())
}

pub fn dismiss(app: &AppHandle) {
    if !app.state::<AppState>().is_busy() {
        set_view(app, View::Idle);
    }
}

pub fn is_running(app: &AppHandle) -> bool {
    matches!(app.state::<AppState>().view(), View::Running { .. })
}

// ---------------------------------------------------------------- the job

struct Throttle(Instant);

impl Throttle {
    fn ready(&mut self) -> bool {
        if self.0.elapsed() >= Duration::from_millis(200) {
            self.0 = Instant::now();
            true
        } else {
            false
        }
    }
}

fn progress(app: &AppHandle, stage: Stage, done: u64, total: u64) {
    let _ = app.emit("progress", Progress { stage, done, total });
}

async fn video_to_png(
    app: &AppHandle,
    m: &Manifest,
    opts: &StartOptions,
    frames_parent: PathBuf,
    video_dir: PathBuf,
    cancel: &CancellationToken,
) -> View {
    let title = m.title.clone();
    let failed = |message: String| View::Failed { message };

    // ---- download
    let (part, final_video) = if opts.keep_video {
        let fin = unique_path(&video_dir, &m.input.filename);
        (with_suffix(&fin, ".part"), Some(fin))
    } else {
        (frames_parent.join(format!(".aaext-{}.part", m.input.filename)), None)
    };
    let mut throttle = Throttle(Instant::now());
    let result = download::download(
        &http_client(),
        &m.input.url,
        &part,
        m.input.size,
        m.input.sha256.as_deref(),
        cancel,
        |done| {
            if throttle.ready() {
                progress(app, Stage::Download, done, m.input.size);
            }
        },
    )
    .await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return match e {
            download::DownloadError::Cancelled => failed("Cancelled.".into()),
            e => failed(e.to_string()),
        };
    }
    let video = match &final_video {
        Some(fin) => match tokio::fs::rename(&part, fin).await {
            Ok(()) => fin.clone(),
            Err(e) => return failed(format!("could not save the video: {e}")),
        },
        None => part.clone(),
    };
    let drop_temp_video = || async {
        if final_video.is_none() {
            let _ = tokio::fs::remove_file(&part).await;
        }
    };

    // ---- probe
    set_view(app, View::Running { title: title.clone(), stage: Stage::Probe });
    let probe = match ffmpeg::probe(&video).await {
        Ok(p) => p,
        Err(e) => {
            drop_temp_video().await;
            return failed(e.to_string());
        }
    };
    let expected = probe.frames.unwrap_or(m.params.frames);
    let plan = ffmpeg::plan(&probe, opts.depth);

    // ---- decode
    let stem = Path::new(&m.input.filename)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "frames".into());
    let frames_dir = unique_dir(&frames_parent, &stem);
    if let Err(e) = tokio::fs::create_dir_all(&frames_dir).await {
        drop_temp_video().await;
        return failed(format!("could not create the frames folder: {e}"));
    }
    set_view(app, View::Running { title: title.clone(), stage: Stage::Decode });
    let mut throttle = Throttle(Instant::now());
    let args = ffmpeg::decode_args(&video, &frames_dir, &plan);
    let result = ffmpeg::decode(args, cancel, |frame| {
        if throttle.ready() {
            progress(app, Stage::Decode, frame, expected);
        }
    })
    .await;

    set_view(app, View::Running { title: title.clone(), stage: Stage::Cleanup });
    drop_temp_video().await;
    let dir = frames_dir.clone();
    let written = tokio::task::spawn_blocking(move || count_frames(&dir)).await.unwrap_or(0);
    let frames_dir_s = frames_dir.display().to_string();

    match result {
        Ok(_) => {
            let mut warnings = Vec::new();
            if written != expected {
                warnings.push(format!("Expected {expected} frames but wrote {written}."));
            }
            if m.params.frames != expected {
                warnings.push(format!(
                    "AAB said {} frames; the file has {expected}.",
                    m.params.frames
                ));
            }
            View::Done {
                title,
                frames_dir: frames_dir_s,
                frames: written,
                expected,
                video: final_video.map(|v| v.display().to_string()),
                warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
            }
        }
        Err(e) => {
            let message = match e {
                ffmpeg::FfmpegError::Cancelled => "Cancelled.".to_string(),
                e => e.to_string(),
            };
            if written == 0 {
                let _ = tokio::fs::remove_dir(&frames_dir).await;
                failed(message)
            } else {
                View::Partial { title, message, written, frames_dir: frames_dir_s }
            }
        }
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// `name`, or `name (2)`, `name (3)`… — the first that does not exist (or is an empty dir).
fn unique_dir(parent: &Path, name: &str) -> PathBuf {
    let empty_or_missing = |p: &Path| match std::fs::read_dir(p) {
        Ok(mut it) => it.next().is_none(),
        Err(_) => !p.exists(),
    };
    let first = parent.join(name);
    if empty_or_missing(&first) {
        return first;
    }
    (2..)
        .map(|n| parent.join(format!("{name} ({n})")))
        .find(|p| empty_or_missing(p))
        .expect("unbounded")
}

fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let first = dir.join(filename);
    if !first.exists() && !with_suffix(&first, ".part").exists() {
        return first;
    }
    let p = Path::new(filename);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists() && !with_suffix(p, ".part").exists())
        .expect("unbounded")
}

fn is_frame_file(name: &str) -> bool {
    name.len() == "frame_000000.png".len()
        && name.starts_with("frame_")
        && name.ends_with(".png")
        && name[6..12].bytes().all(|b| b.is_ascii_digit())
}

fn count_frames(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(Result::ok)
                .filter(|e| is_frame_file(&e.file_name().to_string_lossy()))
                .count() as u64
        })
        .unwrap_or(0)
}

/// Deletes only files this app wrote, then the folder if it ended up empty.
fn remove_frames(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)?.filter_map(Result::ok) {
        if is_frame_file(&entry.file_name().to_string_lossy()) {
            std::fs::remove_file(entry.path())?;
        }
    }
    let _ = std::fs::remove_dir(dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(unique_dir(d.path(), "shot"), d.path().join("shot"));
        std::fs::create_dir(d.path().join("shot")).unwrap();
        assert_eq!(unique_dir(d.path(), "shot"), d.path().join("shot"));
        std::fs::write(d.path().join("shot/x"), b"").unwrap();
        assert_eq!(unique_dir(d.path(), "shot"), d.path().join("shot (2)"));

        std::fs::write(d.path().join("a.mov"), b"").unwrap();
        assert_eq!(unique_path(d.path(), "a.mov"), d.path().join("a (2).mov"));
    }

    #[test]
    fn removes_only_frames() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        std::fs::create_dir(&f).unwrap();
        std::fs::write(f.join("frame_000001.png"), b"").unwrap();
        std::fs::write(f.join("notes.txt"), b"").unwrap();
        assert_eq!(count_frames(&f), 1);
        remove_frames(&f).unwrap();
        assert!(f.join("notes.txt").exists());
        assert!(!f.join("frame_000001.png").exists());
    }
}
