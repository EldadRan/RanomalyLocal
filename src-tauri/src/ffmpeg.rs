//! ffprobe / ffmpeg sidecars: probing, colour-safe RGB conversion and progress parsing.
//!
//! Arguments are always passed as an array; nothing goes through a shell.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum FfmpegError {
    #[error("cancelled")]
    Cancelled,
    #[error("could not start {0}: {1}")]
    Spawn(&'static str, std::io::Error),
    #[error("{0}")]
    Failed(String),
}

/// Sidecars sit next to the app executable (Tauri `externalBin`, suffix stripped at bundle time).
#[cfg(not(test))]
pub fn sidecar(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Tests run from target/*/deps, so they use the source copies in src-tauri/binaries.
#[cfg(test)]
pub fn sidecar(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries").join(format!(
        "{name}-{}{}",
        env!("TARGET_TRIPLE"),
        std::env::consts::EXE_SUFFIX
    ))
}

pub(crate) fn command(name: &str) -> Command {
    let mut cmd = Command::new(sidecar(name));
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `file:` stops ffmpeg from reading `C:` or `name:with:colons` as a protocol.
pub(crate) fn file_arg(path: &Path) -> String {
    format!("file:{}", path.display())
}

// ---------------------------------------------------------------- probe

#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub pix_fmt: String,
    pub fps: Option<f64>,
    /// Frame count reported by the container, if any.
    pub frames: Option<u64>,
    pub duration: Option<f64>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub bit_depth: u8,
    pub has_alpha: bool,
}

#[derive(Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<StreamJson>,
    #[serde(default)]
    format: Option<FormatJson>,
}

#[derive(Deserialize)]
struct StreamJson {
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    nb_frames: Option<String>,
    duration: Option<String>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
}

#[derive(Deserialize)]
struct FormatJson {
    duration: Option<String>,
}

pub async fn probe(input: &Path) -> Result<Probe, FfmpegError> {
    let out = command("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg(
            "stream=codec_name,width,height,pix_fmt,avg_frame_rate,r_frame_rate,nb_frames,\
             duration,color_range,color_space,color_transfer,color_primaries:format=duration",
        )
        .args(["-of", "json"])
        .arg(file_arg(input))
        .output()
        .await
        .map_err(|e| FfmpegError::Spawn("ffprobe", e))?;
    if !out.status.success() {
        return Err(FfmpegError::Failed(format!(
            "ffprobe could not read the video: {}",
            last_lines(&String::from_utf8_lossy(&out.stderr), 3)
        )));
    }
    let json: ProbeJson = serde_json::from_slice(&out.stdout)
        .map_err(|e| FfmpegError::Failed(format!("unexpected ffprobe output: {e}")))?;
    let s = json
        .streams
        .into_iter()
        .next()
        .ok_or_else(|| FfmpegError::Failed("the file has no video stream".into()))?;
    let pix_fmt = s.pix_fmt.clone().unwrap_or_default();
    let (bit_depth, has_alpha) = pix_fmt_info(&pix_fmt)
        .await
        .ok_or_else(|| FfmpegError::Failed(format!("unknown pixel format \"{pix_fmt}\"")))?;
    let known = |v: Option<String>| v.filter(|v| !v.is_empty() && v != "unknown" && v != "unspecified");
    Ok(Probe {
        codec: s.codec_name.unwrap_or_default(),
        width: s.width.unwrap_or(0),
        height: s.height.unwrap_or(0),
        fps: s.avg_frame_rate.as_deref().and_then(parse_rate)
            .or_else(|| s.r_frame_rate.as_deref().and_then(parse_rate)),
        frames: s.nb_frames.and_then(|n| n.parse().ok()).filter(|&n| n > 0),
        duration: s.duration.or(json.format.and_then(|f| f.duration)).and_then(|d| d.parse().ok()),
        color_range: known(s.color_range),
        color_space: known(s.color_space),
        color_transfer: known(s.color_transfer),
        color_primaries: known(s.color_primaries),
        pix_fmt,
        bit_depth,
        has_alpha,
    })
}

fn parse_rate(r: &str) -> Option<f64> {
    let (n, d) = r.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (n > 0.0 && d > 0.0).then(|| n / d)
}

/// Max component bit depth and alpha, from ffprobe's own pixel-format table.
async fn pix_fmt_info(name: &str) -> Option<(u8, bool)> {
    let out = command("ffprobe").args(["-v", "error", "-pix_fmts"]).output().await.ok()?;
    parse_pix_fmts(&String::from_utf8_lossy(&out.stdout), name)
}

/// Rows look like `IO... yuv422p10le            3            20      10-10-10`.
fn parse_pix_fmts(table: &str, name: &str) -> Option<(u8, bool)> {
    table.lines().find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 || cols[1] != name {
            return None;
        }
        let components: u8 = cols[2].parse().ok()?;
        let depth = cols[4].split('-').filter_map(|d| d.parse::<u8>().ok()).max()?;
        Some((depth, components == 2 || components == 4))
    })
}

/// Rough (high bit depth, alpha) guess from a pix_fmt name, for estimates before the file
/// exists. The real values come from `probe` once it is downloaded.
pub fn guess_depth(pix_fmt: &str) -> (bool, bool) {
    let n = pix_fmt.trim_end_matches("le").trim_end_matches("be");
    let digits: String = {
        let rev: String = n.chars().rev().take_while(char::is_ascii_digit).collect();
        rev.chars().rev().collect()
    };
    let bits: u32 = digits.parse().unwrap_or(0);
    let packed_rgb = ["rgb", "bgr", "argb", "abgr"].iter().any(|p| n.starts_with(p));
    let high = if n.starts_with("nv") {
        false
    } else if packed_rgb {
        bits >= 48
    } else {
        (9..=16).contains(&bits)
    };
    let alpha = ["yuva", "rgba", "bgra", "argb", "abgr", "gbrap", "ya"].iter().any(|p| n.starts_with(p));
    (high, alpha)
}

// ---------------------------------------------------------------- decode

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    Match,
    Eight,
    Sixteen,
}

pub struct DecodePlan {
    pub out_pix_fmt: &'static str,
    pub filter: String,
}

/// Explicit YUV→RGB conversion so swscale never falls back to a guessed matrix or range.
pub fn plan(probe: &Probe, depth: Depth) -> DecodePlan {
    let sixteen_bit = match depth {
        Depth::Match => probe.bit_depth > 8,
        Depth::Eight => false,
        Depth::Sixteen => true,
    };
    let out_pix_fmt = match (sixteen_bit, probe.has_alpha) {
        (false, false) => "rgb24",
        (false, true) => "rgba",
        (true, false) => "rgb48be",
        (true, true) => "rgba64be",
    };
    let flags = "flags=accurate_rnd+full_chroma_int+full_chroma_inp";
    let filter = if is_rgb_or_gray(&probe.pix_fmt) {
        format!("scale={flags},format={out_pix_fmt}")
    } else {
        let matrix = match probe.color_space.as_deref() {
            Some("bt709") => "bt709",
            Some("smpte170m" | "bt470bg") => "bt601",
            Some("bt2020nc" | "bt2020c") => "bt2020",
            Some("smpte240m") => "smpte240m",
            Some("fcc") => "fcc",
            // Same default ffmpeg and most players use for untagged video.
            _ if probe.height >= 720 => "bt709",
            _ => "bt601",
        };
        let range = match probe.color_range.as_deref() {
            Some("pc") => "pc",
            _ if probe.pix_fmt.starts_with("yuvj") => "pc",
            _ => "tv",
        };
        format!(
            "scale=in_color_matrix={matrix}:in_range={range}:out_range=pc:{flags},format={out_pix_fmt}"
        )
    };
    DecodePlan { out_pix_fmt, filter }
}

fn is_rgb_or_gray(pix_fmt: &str) -> bool {
    ["rgb", "bgr", "argb", "abgr", "gbr", "0rgb", "0bgr", "x2rgb", "x2bgr", "gray", "ya", "pal8", "monob", "monow"]
        .iter()
        .any(|p| pix_fmt.starts_with(p))
}

/// ffmpeg's image2 muxer expands `%` in the whole output path, so literal ones are doubled.
fn frame_pattern(dir: &Path) -> String {
    let dir = dir.display().to_string().replace('%', "%%");
    let sep = std::path::MAIN_SEPARATOR;
    format!("file:{dir}{sep}frame_%06d.png")
}

pub fn decode_args(input: &Path, out_dir: &Path, plan: &DecodePlan) -> Vec<String> {
    let mut args: Vec<String> = [
        "-hide_banner", "-nostats", "-nostdin", "-v", "error", "-n",
        "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(file_arg(input));
    args.extend(
        [
            "-map", "0:v:0", "-an", "-sn", "-dn",
            "-fps_mode", "passthrough",
            "-vf", &plan.filter,
            "-pix_fmt", plan.out_pix_fmt,
            "-c:v", "png", "-compression_level", "3",
            "-start_number", "1",
            "-progress", "pipe:1",
            "-f", "image2",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(frame_pattern(out_dir));
    args
}

/// Runs ffmpeg, reporting the latest `frame=` count. Returns the last reported frame count.
pub async fn decode(
    args: Vec<String>,
    cancel: &CancellationToken,
    mut on_frame: impl FnMut(u64),
) -> Result<u64, FfmpegError> {
    let mut child = command("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FfmpegError::Spawn("ffmpeg", e))?;

    let stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut tail = VecDeque::with_capacity(20);
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tail.len() == 20 {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        tail.into_iter().collect::<Vec<_>>().join("\n")
    });

    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut frame = 0u64;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // ffmpeg spawns no children of its own, so killing it ends the whole tree.
                let _ = child.kill().await;
                return Err(FfmpegError::Cancelled);
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(n) = line.strip_prefix("frame=").and_then(|n| n.trim().parse().ok()) {
                        frame = n;
                        on_frame(frame);
                    }
                }
                _ => break,
            },
        }
    }
    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(FfmpegError::Cancelled);
        }
        s = child.wait() => s.map_err(|e| FfmpegError::Spawn("ffmpeg", e))?,
    };
    let stderr = err_task.await.unwrap_or_default();
    if !status.success() {
        return Err(FfmpegError::Failed(format!(
            "ffmpeg failed ({}): {}",
            status.code().map_or("killed".into(), |c| c.to_string()),
            last_lines(&stderr, 3)
        )));
    }
    Ok(frame)
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(pix_fmt: &str, space: Option<&str>, range: Option<&str>, depth: u8, alpha: bool) -> Probe {
        Probe {
            codec: "prores".into(), width: 7680, height: 4320, pix_fmt: pix_fmt.into(),
            fps: Some(24.0), frames: Some(10), duration: None,
            color_range: range.map(Into::into), color_space: space.map(Into::into),
            color_transfer: None, color_primaries: None, bit_depth: depth, has_alpha: alpha,
        }
    }

    #[test]
    fn ten_bit_matches_to_rgb48() {
        let p = plan(&probe("yuv422p10le", Some("bt709"), Some("tv"), 10, false), Depth::Match);
        assert_eq!(p.out_pix_fmt, "rgb48be");
        assert!(p.filter.contains("in_color_matrix=bt709:in_range=tv:out_range=pc"));
    }

    #[test]
    fn alpha_and_forced_depths() {
        let pr = probe("yuva444p12le", None, None, 12, true);
        assert_eq!(plan(&pr, Depth::Match).out_pix_fmt, "rgba64be");
        assert_eq!(plan(&pr, Depth::Eight).out_pix_fmt, "rgba");
        let pr = probe("yuv420p", Some("smpte170m"), Some("pc"), 8, false);
        let p = plan(&pr, Depth::Sixteen);
        assert_eq!(p.out_pix_fmt, "rgb48be");
        assert!(p.filter.contains("in_color_matrix=bt601:in_range=pc"));
    }

    #[test]
    fn rgb_sources_skip_matrix() {
        let p = plan(&probe("gbrp10le", None, None, 10, false), Depth::Match);
        assert!(!p.filter.contains("in_color_matrix"));
    }

    #[test]
    fn guesses_depth_from_names() {
        for (name, want) in [
            ("yuv420p", (false, false)), ("yuv422p10le", (true, false)), ("p010le", (true, false)),
            ("nv12", (false, false)), ("rgb24", (false, false)), ("rgb48be", (true, false)),
            ("rgba64le", (true, true)), ("yuva444p12le", (true, true)), ("gbrp16le", (true, false)),
            ("yuv410p", (false, false)), ("", (false, false)),
        ] {
            assert_eq!(guess_depth(name), want, "{name}");
        }
    }

    #[test]
    fn parses_pix_fmt_table() {
        let t = "Pixel formats:\n-----\nIO... yuv420p                3             12      8-8-8\n\
                 IO... yuv422p10le            3             20      10-10-10\n\
                 IO... yuva444p12le           4             48      12-12-12-12\n\
                 IO... nv12                   3             12      8-8-8\n";
        assert_eq!(parse_pix_fmts(t, "yuv422p10le"), Some((10, false)));
        assert_eq!(parse_pix_fmts(t, "yuva444p12le"), Some((12, true)));
        assert_eq!(parse_pix_fmts(t, "nv12"), Some((8, false)));
        assert_eq!(parse_pix_fmts(t, "nope"), None);
    }

    #[test]
    fn escapes_percent_and_uses_file_protocol() {
        let args = decode_args(
            Path::new("/in/a:b 100%.mov"),
            Path::new("/out/100% done ü"),
            &plan(&probe("yuv420p", None, None, 8, false), Depth::Match),
        );
        assert!(args.contains(&"file:/in/a:b 100%.mov".to_string()));
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(args.last().unwrap(), &format!("file:/out/100%% done ü{sep}frame_%06d.png"));
    }
}

/// End-to-end against the real sidecars in src-tauri/binaries.
#[cfg(test)]
mod sidecar_tests {
    use super::*;

    /// A solid-colour clip, encoded the way a camera/NLE would: 10-bit 4:2:2, BT.709, TV range.
    async fn make_clip(dir: &Path, rgb: &str, seconds: u32, size: &str) -> PathBuf {
        let clip = dir.join("clip 100% ü.mov");
        let out = command("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("color=c=0x{rgb}:s={size}:r=24:d={seconds}"))
            .args([
                "-vf", "format=rgb24,scale=out_color_matrix=bt709:out_range=tv,format=yuv422p10le",
                "-c:v", "prores_ks", "-profile:v", "3",
                "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709",
                "-color_range", "tv",
            ])
            .arg(file_arg(&clip))
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        clip
    }

    fn centre_pixel16(png_path: &Path) -> [u16; 3] {
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(png_path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
        assert_eq!(info.color_type, png::ColorType::Rgb);
        let (w, h) = (info.width as usize, info.height as usize);
        let i = ((h / 2) * w + w / 2) * 6;
        let px = |o: usize| u16::from_be_bytes([buf[i + o], buf[i + o + 1]]);
        [px(0), px(2), px(4)]
    }

    fn max_err(px: [u16; 3], rgb8: [u8; 3]) -> i32 {
        (0..3).map(|c| (px[c] as i32 - rgb8[c] as i32 * 257).abs()).max().unwrap()
    }

    #[tokio::test]
    async fn ten_bit_clip_decodes_to_colour_accurate_16_bit_pngs() {
        let dir = tempfile::tempdir().unwrap();
        let clip = make_clip(dir.path(), "B4103C", 1, "320x240").await;
        let p = probe(&clip).await.unwrap();
        assert_eq!((p.pix_fmt.as_str(), p.bit_depth, p.has_alpha), ("yuv422p10le", 10, false));
        assert_eq!(p.color_space.as_deref(), Some("bt709"));
        assert_eq!(p.frames, Some(24));

        let out = dir.path().join("frames 100%");
        std::fs::create_dir(&out).unwrap();
        let plan = plan(&p, Depth::Match);
        let last = decode(decode_args(&clip, &out, &plan), &CancellationToken::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(last, 24);
        let mut names: Vec<_> = std::fs::read_dir(&out).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 24);
        assert_eq!(names[0], "frame_000001.png");
        assert_eq!(names[23], "frame_000024.png");

        // 10-bit quantisation of a TV-range signal is worth ~2 codes at 8 bits; allow 1%.
        let px = centre_pixel16(&out.join("frame_000001.png"));
        let err = max_err(px, [0xB4, 0x10, 0x3C]);
        assert!(err < 655, "decoded {px:?}, error {err}");

        // The same frame read with the wrong matrix is visibly off, so the check above
        // actually distinguishes a colour shift.
        let wrong = dir.path().join("wrong");
        std::fs::create_dir(&wrong).unwrap();
        let bad = plan.filter.replace("in_color_matrix=bt709", "in_color_matrix=bt601");
        let bad_plan = DecodePlan { out_pix_fmt: plan.out_pix_fmt, filter: bad };
        decode(decode_args(&clip, &wrong, &bad_plan), &CancellationToken::new(), |_| {}).await.unwrap();
        let bad_err = max_err(centre_pixel16(&wrong.join("frame_000001.png")), [0xB4, 0x10, 0x3C]);
        eprintln!("colour error (16-bit units): correct matrix {err}, wrong matrix {bad_err}");
        assert!(bad_err > 5 * err.max(200), "wrong matrix error {bad_err} vs {err}");
    }

    #[tokio::test]
    async fn cancel_stops_ffmpeg_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let clip = make_clip(dir.path(), "808080", 20, "1920x1080").await;
        let p = probe(&clip).await.unwrap();
        let out = dir.path().join("f");
        std::fs::create_dir(&out).unwrap();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        let started = std::time::Instant::now();
        let r = decode(decode_args(&clip, &out, &plan(&p, Depth::Match)), &cancel, move |f| {
            if f > 0 {
                c.cancel();
            }
        })
        .await;
        assert!(matches!(r, Err(FfmpegError::Cancelled)), "{r:?}");
        let written = std::fs::read_dir(&out).unwrap().count();
        assert!(written > 0 && written < 480, "{written}");
        // No stray ffmpeg writing more frames after cancel returned.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), written);
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }
}
