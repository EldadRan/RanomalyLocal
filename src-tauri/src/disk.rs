//! Free-space check before a job starts.

use std::path::Path;

use serde::Serialize;

use crate::ops::video_to_png::Params;

/// PNG size as a fraction of the raw RGB(A) frame. Film/VFX plates at compression 3 land
/// roughly here; the low bound blocks, the high bound warns.
const PNG_RATIO_LOW: f64 = 0.35;
const PNG_RATIO_HIGH: f64 = 0.75;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Ok,
    Warn,
    Block,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskCheck {
    pub verdict: Verdict,
    pub frames_low: u64,
    pub frames_high: u64,
    pub video: u64,
    /// One entry per volume involved.
    pub volumes: Vec<VolumeCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VolumeCheck {
    pub path: String,
    pub free: u64,
    pub need_low: u64,
    pub need_high: u64,
}

/// Estimated PNG bytes per frame (low, high).
pub fn frame_bytes(params: &Params, sixteen_bit: bool, alpha: bool) -> (u64, u64) {
    let channels = if alpha { 4.0 } else { 3.0 };
    let bytes = if sixteen_bit { 2.0 } else { 1.0 };
    let raw = params.width as f64 * params.height as f64 * channels * bytes;
    ((raw * PNG_RATIO_LOW) as u64, (raw * PNG_RATIO_HIGH) as u64)
}

pub fn check(
    params: &Params,
    video_size: u64,
    frames_parent: &Path,
    video_dir: &Path,
    sixteen_bit: bool,
    alpha: bool,
) -> std::io::Result<DiskCheck> {
    let (per_low, per_high) = frame_bytes(params, sixteen_bit, alpha);
    let frames_low = per_low.saturating_mul(params.frames);
    let frames_high = per_high.saturating_mul(params.frames);

    let mut volumes = vec![VolumeCheck {
        path: frames_parent.display().to_string(),
        free: fs2::available_space(frames_parent)?,
        need_low: frames_low,
        need_high: frames_high,
    }];
    if same_volume(frames_parent, video_dir)? {
        volumes[0].need_low += video_size;
        volumes[0].need_high += video_size;
    } else {
        volumes.push(VolumeCheck {
            path: video_dir.display().to_string(),
            free: fs2::available_space(video_dir)?,
            need_low: video_size,
            need_high: video_size,
        });
    }

    let verdict = if volumes.iter().any(|v| v.free < v.need_low) {
        Verdict::Block
    } else if volumes.iter().any(|v| v.free < v.need_high) {
        Verdict::Warn
    } else {
        Verdict::Ok
    };
    Ok(DiskCheck { verdict, frames_low, frames_high, video: video_size, volumes })
}

#[cfg(unix)]
fn same_volume(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(a)?.dev() == std::fs::metadata(b)?.dev())
}

#[cfg(windows)]
fn same_volume(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::path::Component;
    let root = |p: &Path| -> std::io::Result<String> {
        let p = std::fs::canonicalize(p)?;
        Ok(match p.components().next() {
            Some(Component::Prefix(pre)) => pre.as_os_str().to_string_lossy().to_lowercase(),
            _ => String::new(),
        })
    };
    Ok(root(a)? == root(b)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_k_estimates_match_the_brief() {
        let p = Params { frames: 1, width: 7680, height: 4320, fps: None, pix_fmt: None };
        let (lo, hi) = frame_bytes(&p, false, false);
        // Brief: roughly 30–80 MB per 8-bit 8K frame.
        assert!((30_000_000..40_000_000).contains(&lo), "{lo}");
        assert!((70_000_000..80_000_000).contains(&hi), "{hi}");
    }

    #[test]
    fn blocks_when_the_volume_is_too_small() {
        let dir = tempfile::tempdir().unwrap();
        let huge = Params { frames: 10_000_000, width: 7680, height: 4320, fps: None, pix_fmt: None };
        let c = check(&huge, 1, dir.path(), dir.path(), true, false).unwrap();
        assert_eq!(c.verdict, Verdict::Block);
        assert_eq!(c.volumes.len(), 1);
        let tiny = Params { frames: 1, width: 8, height: 8, fps: None, pix_fmt: None };
        assert_eq!(check(&tiny, 1, dir.path(), dir.path(), true, false).unwrap().verdict, Verdict::Ok);
    }
}
