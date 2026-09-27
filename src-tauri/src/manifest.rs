//! The job manifest AAB writes to R2 (see docs/manifest.md).

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{config, link};

#[derive(Debug, Clone, Deserialize)]
struct RawManifest {
    version: u32,
    op: String,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    input: RawInput,
    params: Params,
}

#[derive(Debug, Clone, Deserialize)]
struct RawInput {
    url: String,
    filename: String,
    size: u64,
    #[serde(default)]
    sha256: Option<String>,
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

/// A manifest that passed validation; `op` is resolved to a known tool.
#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    pub op: Op,
    pub job_id: Option<String>,
    pub title: String,
    pub input: Input,
    pub params: Params,
    /// Unix seconds when the video link stops working, if the URL says.
    pub input_expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Input {
    #[serde(skip)]
    pub url: Url,
    pub filename: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    VideoToPng,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("the request link has expired — start again from AAB")]
    Expired,
    #[error("could not fetch the request: {0}")]
    Fetch(String),
    #[error("the request is not valid: {0}")]
    Invalid(String),
    #[error("this request needs a newer version of Ranomaly Ext ({0})")]
    Unsupported(String),
}

pub fn parse(bytes: &[u8]) -> Result<Manifest, ManifestError> {
    let invalid = |m: &str| ManifestError::Invalid(m.to_string());
    let raw: RawManifest =
        serde_json::from_slice(bytes).map_err(|e| ManifestError::Invalid(e.to_string()))?;
    if raw.version != 1 {
        return Err(ManifestError::Unsupported(format!("manifest version {}", raw.version)));
    }
    let op = match raw.op.as_str() {
        "video_to_png" => Op::VideoToPng,
        other => return Err(ManifestError::Unsupported(format!("tool \"{other}\""))),
    };
    let url = Url::parse(&raw.input.url).map_err(|_| invalid("input.url is not a URL"))?;
    link::check_allowed(&url).map_err(|_| invalid("input.url points outside AAB storage"))?;
    let filename = check_filename(&raw.input.filename).ok_or_else(|| invalid("input.filename"))?;
    if raw.input.size == 0 {
        return Err(invalid("input.size must be greater than 0"));
    }
    let sha256 = match raw.input.sha256 {
        Some(h) if h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()) => {
            Some(h.to_ascii_lowercase())
        }
        Some(_) => return Err(invalid("input.sha256 must be 64 hex characters")),
        None => None,
    };
    let p = &raw.params;
    if p.frames == 0 || p.width == 0 || p.height == 0 || p.width > 65536 || p.height > 65536 {
        return Err(invalid("params.frames, width and height must be positive"));
    }
    let title = raw
        .title
        .map(|t| t.chars().filter(|c| !c.is_control()).take(200).collect::<String>())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| filename.clone());
    let job_id = raw
        .job_id
        .map(|j| j.chars().filter(|c| !c.is_control()).take(100).collect());
    Ok(Manifest {
        op,
        job_id,
        title,
        input_expires_at: link::presigned_expiry(&url),
        input: Input { url, filename, size: raw.input.size, sha256 },
        params: raw.params,
    })
}

/// Rejects anything that is not a plain, portable file name.
fn check_filename(name: &str) -> Option<String> {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let bad_char = |c: char| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty()
        || name.len() > 200
        || name.chars().any(bad_char)
        || name.starts_with('.')
        || name.ends_with('.')
        || name.ends_with(' ')
        || RESERVED.contains(&stem.as_str())
    {
        return None;
    }
    Some(name.to_string())
}

pub async fn fetch(client: &reqwest::Client, url: &Url) -> Result<Manifest, ManifestError> {
    let resp = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| ManifestError::Fetch(e.without_url().to_string()))?;
    match resp.status().as_u16() {
        200 => {}
        403 => return Err(ManifestError::Expired),
        404 => return Err(ManifestError::Fetch("the request was not found (404)".into())),
        s => return Err(ManifestError::Fetch(format!("server answered {s}"))),
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ManifestError::Fetch(e.without_url().to_string()))?;
        if body.len() + chunk.len() > config::MAX_MANIFEST_BYTES {
            return Err(ManifestError::Invalid("manifest is too large".into()));
        }
        body.extend_from_slice(&chunk);
    }
    parse(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(input_url: &str, filename: &str) -> String {
        serde_json::json!({
            "version": 1, "op": "video_to_png", "job_id": "j_1",
            "input": {"url": input_url, "filename": filename, "size": 10,
                      "sha256": "AB".repeat(32)},
            "params": {"frames": 3, "width": 8, "height": 8, "fps": 24, "pix_fmt": "yuv420p"},
            "future_field": {"ignored": true}
        })
        .to_string()
    }

    fn r2(path: &str) -> String {
        format!("https://{}{}", config::r2_host(), path)
    }

    #[test]
    fn parses_valid_manifest() {
        let m = parse(manifest(&r2("/aab-media/a.mov"), "shot 01 ü.mov").as_bytes()).unwrap();
        assert_eq!(m.op, Op::VideoToPng);
        assert_eq!(m.title, "shot 01 ü.mov");
        assert_eq!(m.input.sha256.as_deref(), Some("ab".repeat(32).as_str()));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(manifest("https://evil.example/aab-media/a.mov", "a.mov").as_bytes()).is_err());
        for name in ["../a.mov", "a/b.mov", "a\\b.mov", "", ".hidden", "CON.mov", "a.mov.", "C:x"] {
            assert!(parse(manifest(&r2("/aab-media/a.mov"), name).as_bytes()).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_unknown_op_and_version() {
        let s = manifest(&r2("/aab-media/a.mov"), "a.mov");
        let op = s.replace("video_to_png", "rm_rf");
        assert!(matches!(parse(op.as_bytes()), Err(ManifestError::Unsupported(_))));
        let v = s.replace("\"version\":1", "\"version\":2");
        assert!(matches!(parse(v.as_bytes()), Err(ManifestError::Unsupported(_))));
    }
}
