//! `video_to_png`: download a video, decode it to a numbered PNG sequence.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

use super::{Ctx, Discard, Fact, Outcome, Unit, CANCELLED};
use crate::ffmpeg::{self, Depth};
use crate::manifest::{self, ManifestError};
use crate::{disk, download, job, link};

pub const OP: &str = "video_to_png";
pub const STAGES: &[&str] = &["Download", "Check", "Decode", "Finish"];
const DOWNLOAD: usize = 0;
const PROBE: usize = 1;
const DECODE: usize = 2;
const FINISH: usize = 3;

// ---------------------------------------------------------------- manifest

#[derive(Deserialize)]
struct Raw {
    input: RawInput,
    params: Params,
}

#[derive(Deserialize)]
struct RawInput {
    url: String,
    filename: String,
    size: u64,
    #[serde(default)]
    sha256: Option<String>,
    /// Unix seconds when `url` stops working. AAB knows it because it chose the lifetime.
    #[serde(default)]
    expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Params {
    pub frames: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub fps: Option<f64>,
    #[serde(default)]
    pub pix_fmt: Option<String>,
}

pub struct Job {
    url: Url,
    filename: String,
    size: u64,
    sha256: Option<String>,
    expires_at: Option<u64>,
    params: Params,
    /// Guesses from the manifest's pix_fmt; the probe decides once the file is here.
    high_bit: bool,
    alpha: bool,
}

impl Job {
    pub fn from_manifest(doc: Value) -> Result<Self, ManifestError> {
        let raw: Raw = serde_json::from_value(doc).map_err(|e| ManifestError::invalid(e.to_string()))?;
        let url = manifest::media_url(&raw.input.url, "input.url")?;
        let filename = manifest::check_filename(&raw.input.filename)
            .ok_or_else(|| ManifestError::invalid("input.filename"))?;
        if raw.input.size == 0 {
            return Err(ManifestError::invalid("input.size must be greater than 0"));
        }
        let sha256 = match raw.input.sha256 {
            Some(h) if h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()) => {
                Some(h.to_ascii_lowercase())
            }
            Some(_) => return Err(ManifestError::invalid("input.sha256 must be 64 hex characters")),
            None => None,
        };
        let p = &raw.params;
        if p.frames == 0 || p.width == 0 || p.height == 0 || p.width > 65536 || p.height > 65536 {
            return Err(ManifestError::invalid("params.frames, width and height must be positive"));
        }
        let (high_bit, alpha) = ffmpeg::guess_depth(p.pix_fmt.as_deref().unwrap_or(""));
        Ok(Job {
            // CF promises no URL format, so the manifest's own figure wins; parsing is a fallback.
            expires_at: raw.input.expires_at.or_else(|| link::presigned_expiry(&url)),
            url,
            filename,
            size: raw.input.size,
            sha256,
            params: raw.params,
            high_bit,
            alpha,
        })
    }

    pub fn default_title(&self) -> String {
        self.filename.clone()
    }

    pub fn details(&self) -> Value {
        let p = &self.params;
        json!({
            "filename": self.filename,
            "size": self.size,
            "expires_at": self.expires_at,
            "frames": p.frames, "width": p.width, "height": p.height,
            "fps": p.fps, "pix_fmt": p.pix_fmt,
            "source_high_bit": self.high_bit,
            "source_alpha": self.alpha,
        })
    }

    // ------------------------------------------------------------ options

    pub fn preflight(&self, opts: &Value) -> Result<Value, String> {
        let opts = Options::parse(opts)?;
        let check = self.disk_check(&opts)?;
        serde_json::to_value(check).map_err(|e| e.to_string())
    }

    pub fn start_check(&self, opts: &Value) -> Result<(), String> {
        let opts = Options::parse(opts)?;
        if self.disk_check(&opts)?.verdict == disk::Verdict::Block {
            return Err("there is not enough free disk space for this job".into());
        }
        Ok(())
    }

    fn disk_check(&self, opts: &Options) -> Result<disk::DiskCheck, String> {
        let sixteen = opts.depth == Depth::Sixteen || (opts.depth == Depth::Match && self.high_bit);
        disk::check(&self.params, self.size, &opts.frames_parent, &opts.video_dir, sixteen, self.alpha)
            .map_err(|e| format!("could not read free space: {e}"))
    }

    // ------------------------------------------------------------ run

    pub async fn run(&self, ctx: &Ctx, opts: Value) -> Outcome {
        let opts = match Options::parse(&opts) {
            Ok(o) => o,
            Err(message) => return Outcome::Failed { message },
        };
        let failed = |message: String| Outcome::Failed { message };

        // ---- download
        let (part, kept_video) = if opts.keep_video {
            let fin = unique_path(&opts.video_dir, &self.filename);
            (with_suffix(&fin, ".part"), Some(fin))
        } else {
            (opts.frames_parent.join(format!(".ranomaly-{}.part", self.filename)), None)
        };
        let mut rep = ctx.reporter(DOWNLOAD, Unit::Bytes);
        let size = self.size;
        let result = download::download(
            &job::http_client(),
            &self.url,
            &part,
            size,
            self.sha256.as_deref(),
            &ctx.cancel,
            |done| rep.report(done, size),
        )
        .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&part).await;
            return match e {
                download::DownloadError::Cancelled => failed(CANCELLED.into()),
                e => failed(e.to_string()),
            };
        }
        let video = match &kept_video {
            Some(fin) => match tokio::fs::rename(&part, fin).await {
                Ok(()) => fin.clone(),
                Err(e) => return failed(format!("could not save the video: {e}")),
            },
            None => part.clone(),
        };
        let drop_temp_video = || async {
            if kept_video.is_none() {
                let _ = tokio::fs::remove_file(&part).await;
            }
        };

        // ---- probe
        ctx.stage(PROBE);
        let probe = match ffmpeg::probe(&video).await {
            Ok(p) => p,
            Err(e) => {
                drop_temp_video().await;
                return failed(e.to_string());
            }
        };
        let expected = probe.frames.unwrap_or(self.params.frames);
        let plan = ffmpeg::plan(&probe, opts.depth);

        // ---- decode
        let stem = Path::new(&self.filename)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "frames".into());
        let frames_dir = unique_dir(&opts.frames_parent, &stem);
        if let Err(e) = tokio::fs::create_dir_all(&frames_dir).await {
            drop_temp_video().await;
            return failed(format!("could not create the frames folder: {e}"));
        }
        ctx.stage(DECODE);
        let mut rep = ctx.reporter(DECODE, Unit::Frames);
        let args = ffmpeg::decode_args(&video, &frames_dir, &plan);
        let result = ffmpeg::decode(args, &ctx.cancel, |frame| rep.report(frame, expected)).await;

        // ---- finish
        ctx.stage(FINISH);
        drop_temp_video().await;
        let dir = frames_dir.clone();
        let frames = tokio::task::spawn_blocking(move || frame_files(&dir)).await.unwrap_or_default();
        let written = frames.len() as u64;
        let folder = Fact::text("Folder", frames_dir.display().to_string());

        match result {
            Ok(_) => {
                let mut warnings = Vec::new();
                if written != expected {
                    warnings.push(format!("Expected {expected} frames but wrote {written}."));
                }
                if self.params.frames != expected {
                    warnings.push(format!("AAB said {} frames; the file has {expected}.", self.params.frames));
                }
                let mut facts = vec![Fact::mono("Frames", written.to_string()), folder];
                if let Some(v) = &kept_video {
                    facts.push(Fact::text("Video", v.display().to_string()));
                }
                Outcome::Done {
                    facts,
                    warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
                    open: Some(frames_dir),
                }
            }
            Err(e) => {
                let message = match e {
                    ffmpeg::FfmpegError::Cancelled => CANCELLED.to_string(),
                    e => e.to_string(),
                };
                if written == 0 {
                    let _ = tokio::fs::remove_dir(&frames_dir).await;
                    return failed(message);
                }
                Outcome::Partial {
                    message,
                    facts: vec![Fact::mono("Written", format!("{written} frames")), folder],
                    discard: Discard { files: frames, dirs: vec![frames_dir.clone()] },
                    open: Some(frames_dir),
                }
            }
        }
    }
}

// ---------------------------------------------------------------- the user's choices

#[derive(Deserialize)]
struct RawOptions {
    frames_parent: String,
    #[serde(default)]
    keep_video: bool,
    #[serde(default)]
    video_dir: Option<String>,
    #[serde(default = "default_depth")]
    depth: Depth,
}

fn default_depth() -> Depth {
    Depth::Eight
}

struct Options {
    frames_parent: PathBuf,
    keep_video: bool,
    /// Where the video goes: the chosen folder if kept, else next to the frames (temporary).
    video_dir: PathBuf,
    depth: Depth,
}

impl Options {
    fn parse(v: &Value) -> Result<Self, String> {
        let raw: RawOptions = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        let frames_parent = PathBuf::from(&raw.frames_parent);
        if !frames_parent.is_dir() {
            return Err("choose a folder for the frames".into());
        }
        let video_dir = match (raw.keep_video, &raw.video_dir) {
            (true, Some(d)) if Path::new(d).is_dir() => PathBuf::from(d),
            (true, _) => return Err("choose a folder for the video".into()),
            (false, _) => frames_parent.clone(),
        };
        Ok(Options { frames_parent, keep_video: raw.keep_video, video_dir, depth: raw.depth })
    }
}

// ---------------------------------------------------------------- files

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
    let free = |p: &Path| !p.exists() && !with_suffix(p, ".part").exists();
    let first = dir.join(filename);
    if free(&first) {
        return first;
    }
    let p = Path::new(filename);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| free(p))
        .expect("unbounded")
}

fn is_frame_file(name: &str) -> bool {
    name.len() == "frame_000000.png".len()
        && name.starts_with("frame_")
        && name.ends_with(".png")
        && name[6..12].bytes().all(|b| b.is_ascii_digit())
}

/// The frames this op wrote into `dir` — nothing else that may be there.
fn frame_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(Result::ok)
                .filter(|e| is_frame_file(&e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(input_url: &str, filename: &str) -> Value {
        json!({
            "version": 1, "op": "video_to_png", "job_id": "j_1",
            "input": {"url": input_url, "filename": filename, "size": 10,
                      "sha256": "AB".repeat(32), "expires_at": 1_790_525_700u64},
            "params": {"frames": 3, "width": 8, "height": 8, "fps": 24, "pix_fmt": "yuv422p10le"},
            "future_field": {"ignored": true}
        })
    }

    const MEDIA: &str = "https://delivery.example.test/v/a.mov";

    #[test]
    fn parses_valid_manifest() {
        let j = Job::from_manifest(manifest(MEDIA, "shot 01 ü.mov")).unwrap();
        assert_eq!(j.default_title(), "shot 01 ü.mov");
        assert_eq!(j.sha256.as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(j.expires_at, Some(1_790_525_700));
        assert!(j.high_bit);
        assert_eq!(j.details()["frames"], 3);
    }

    /// Verbatim from AA Base's mockup server (AAB_design/mockup/serve.py, 2026-09-27): the shape
    /// AAB actually sends must keep parsing.
    #[test]
    fn parses_aab_mockup_manifest() {
        let raw = br#"{"version": 1, "op": "video_to_png", "job_id": "ef38be1f59d35899fd8e4937ba06e6ac",
            "title": "Harbour plate \u2014 \u21163",
            "input": {"url": "http://127.0.0.1:8741/aab-media/sample/proxy.mp4",
                      "filename": "Harbour plate.mp4", "size": 1095180, "expires_at": 1790529650},
            "params": {"frames": 192, "width": 1280, "height": 720, "fps": 24}}"#;
        let (env, doc) = manifest::parse(raw).unwrap();
        let job = match crate::ops::prepare(&env, doc).unwrap() {
            crate::ops::Job::VideoToPng(j) => j,
        };
        assert_eq!(env.title.as_deref(), Some("Harbour plate — №3"));
        assert_eq!((job.size, job.params.frames, job.expires_at), (1095180, 192, Some(1790529650)));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Job::from_manifest(manifest("https://evil.example/a.mov", "a.mov")).is_err());
        assert!(Job::from_manifest(manifest(MEDIA, "../a.mov")).is_err());
        let mut m = manifest(MEDIA, "a.mov");
        m["params"]["frames"] = json!(0);
        assert!(Job::from_manifest(m).is_err());
        let mut m = manifest(MEDIA, "a.mov");
        m["input"].as_object_mut().unwrap().remove("size");
        assert!(Job::from_manifest(m).is_err());
    }

    #[test]
    fn options_require_existing_folders() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().display().to_string();
        assert!(Options::parse(&json!({"frames_parent": p})).is_ok());
        assert_eq!(Options::parse(&json!({"frames_parent": p})).unwrap().depth, Depth::Eight);
        assert!(Options::parse(&json!({"frames_parent": "/nope/nope"})).is_err());
        assert!(Options::parse(&json!({"frames_parent": p, "keep_video": true})).is_err());
    }

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
    fn lists_only_frames() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("frame_000001.png"), b"").unwrap();
        std::fs::write(d.path().join("frame_1.png"), b"").unwrap();
        std::fs::write(d.path().join("notes.txt"), b"").unwrap();
        assert_eq!(frame_files(d.path()), vec![d.path().join("frame_000001.png")]);
    }
}

/// The whole op through the shell's contract: HTTP download → real ffprobe/ffmpeg → Outcome.
#[cfg(test)]
mod e2e {
    use super::*;
    use crate::ops::{Ctx, Events, Progress};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    async fn clip(dir: &Path) -> PathBuf {
        let clip = dir.join("src.mov");
        let out = ffmpeg::command("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc2=s=640x360:r=24:d=2",
                   "-vf", "format=yuv422p10le", "-c:v", "prores_ks", "-colorspace", "bt709"])
            .arg(ffmpeg::file_arg(&clip))
            .output()
            .await
            .unwrap();
        assert!(out.status.success());
        clip
    }

    /// Serves one file at /v/<name> (no Range; enough for a clean run).
    async fn serve(bytes: Vec<u8>) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let body = std::sync::Arc::new(bytes);
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf).await;
                    let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len());
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&body).await;
                });
            }
        });
        format!("http://{addr}/v/shot%20ü.mov")
    }

    /// Records what the window would have been told.
    #[derive(Default)]
    struct Recorder {
        stages: Mutex<Vec<usize>>,
        last: Mutex<Option<Progress>>,
    }

    impl Events for Recorder {
        fn stage(&self, i: usize) {
            self.stages.lock().unwrap().push(i);
        }
        fn progress(&self, p: Progress) {
            *self.last.lock().unwrap() = Some(p);
        }
    }

    fn ctx(cancel: CancellationToken) -> (Ctx, Arc<Recorder>) {
        let rec = Arc::new(Recorder::default());
        (Ctx { cancel, events: rec.clone() }, rec)
    }

    async fn job(dir: &Path) -> (Job, u64) {
        let bytes = std::fs::read(clip(dir).await).unwrap();
        let size = bytes.len() as u64;
        let url = serve(bytes).await;
        let doc = json!({
            "version": 1, "op": OP,
            "input": {"url": url, "filename": "shot ü.mov", "size": size},
            "params": {"frames": 48, "width": 640, "height": 360}
        });
        (Job::from_manifest(doc).unwrap(), size)
    }

    #[tokio::test]
    async fn runs_to_done_and_cleans_its_temp_video() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let (job, _) = job(d.path()).await;
        let opts = json!({"frames_parent": out.display().to_string(), "depth": "sixteen"});
        job.start_check(&opts).unwrap();
        let (ctx, rec) = ctx(CancellationToken::new());
        let outcome = job.run(&ctx, opts).await;
        let Outcome::Done { facts, warning, open } = outcome else { panic!("not done") };
        assert_eq!(*rec.stages.lock().unwrap(), vec![PROBE, DECODE, FINISH]);
        let last = rec.last.lock().unwrap().clone().unwrap();
        assert_eq!((last.stage, last.done, last.total), (DECODE, 48, 48));
        assert_eq!(warning, None);
        assert_eq!(facts[0].value, "48");
        let frames_dir = open.unwrap();
        assert_eq!(frames_dir, out.join("shot ü"));
        assert_eq!(frame_files(&frames_dir).len(), 48);
        // Only the frames folder is left: the temporary download is gone.
        let left: Vec<_> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("shot ü")]);
    }

    #[tokio::test]
    async fn keeps_the_video_when_asked() {
        let d = tempfile::tempdir().unwrap();
        let (out, vids) = (d.path().join("out"), d.path().join("vids"));
        std::fs::create_dir(&out).unwrap();
        std::fs::create_dir(&vids).unwrap();
        let (job, size) = job(d.path()).await;
        let opts = json!({"frames_parent": out.display().to_string(), "keep_video": true,
                          "video_dir": vids.display().to_string()});
        let Outcome::Done { facts, .. } = job.run(&ctx(CancellationToken::new()).0, opts).await else {
            panic!("not done")
        };
        assert_eq!(std::fs::metadata(vids.join("shot ü.mov")).unwrap().len(), size);
        assert!(facts.iter().any(|f| f.label == "Video"));
    }

    #[tokio::test]
    async fn cancel_before_decode_leaves_nothing() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let (job, _) = job(d.path()).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = job.run(&ctx(cancel).0, json!({"frames_parent": out.display().to_string()})).await;
        assert!(matches!(outcome, Outcome::Failed { ref message } if message == CANCELLED));
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
    }
}
