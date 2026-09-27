//! The manifest envelope every op shares (see docs/manifest.md).
//!
//! Only `version`, `op`, `job_id` and `title` belong to the envelope. Everything else in the JSON
//! is the op's own, and the op parses it from the same document (`ops::prepare`).

use futures_util::StreamExt;
use serde::Deserialize;
use url::Url;

use crate::{config, link};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub op: String,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
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

impl ManifestError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}

/// Parses the envelope and hands back the whole document for the op to parse.
pub fn parse(bytes: &[u8]) -> Result<(Envelope, serde_json::Value), ManifestError> {
    let doc: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| ManifestError::invalid(e.to_string()))?;
    let env: Envelope =
        serde_json::from_value(doc.clone()).map_err(|e| ManifestError::invalid(e.to_string()))?;
    if env.version != VERSION {
        return Err(ManifestError::Unsupported(format!("manifest version {}", env.version)));
    }
    Ok((env, doc))
}

pub async fn fetch(
    client: &reqwest::Client,
    url: &Url,
) -> Result<(Envelope, serde_json::Value), ManifestError> {
    let resp = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| ManifestError::Fetch(e.without_url().to_string()))?;
    match resp.status().as_u16() {
        200 => {}
        // Expired presigned URL, or a one-time manifest already used or timed out.
        403 | 404 | 410 => return Err(ManifestError::Expired),
        s => return Err(ManifestError::Fetch(format!("server answered {s}"))),
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ManifestError::Fetch(e.without_url().to_string()))?;
        if body.len() + chunk.len() > config::MAX_MANIFEST_BYTES {
            return Err(ManifestError::invalid("manifest is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    parse(&body)
}

// ---------------------------------------------------------------- helpers for ops

/// A job input URL: parsed and inside `config::MEDIA_ORIGINS`.
pub fn media_url(raw: &str, field: &str) -> Result<Url, ManifestError> {
    let url = Url::parse(raw).map_err(|_| ManifestError::invalid(format!("{field} is not a URL")))?;
    link::check_allowed(&url, config::MEDIA_ORIGINS)
        .map_err(|_| ManifestError::invalid(format!("{field} points outside the allowed locations")))?;
    Ok(url)
}

/// Short display text: control characters removed, length capped, blank → None.
pub fn display_text(s: Option<String>, max: usize) -> Option<String> {
    s.map(|t| t.chars().filter(|c| !c.is_control()).take(max).collect::<String>())
        .filter(|t| !t.trim().is_empty())
}

/// Rejects anything that is not a plain, portable file name.
pub fn check_filename(name: &str) -> Option<String> {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let bad_char =
        |c: char| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_envelope_and_keeps_the_document() {
        let (env, doc) =
            parse(br#"{"version":1,"op":"anything","job_id":"j","extra":{"a":1}}"#).unwrap();
        assert_eq!(env.op, "anything");
        assert_eq!(doc["extra"]["a"], 1);
    }

    #[test]
    fn rejects_other_versions_and_garbage() {
        assert!(matches!(parse(br#"{"version":2,"op":"x"}"#), Err(ManifestError::Unsupported(_))));
        assert!(matches!(parse(b"not json"), Err(ManifestError::Invalid(_))));
        assert!(matches!(parse(br#"{"version":1}"#), Err(ManifestError::Invalid(_))));
    }

    #[test]
    fn filenames() {
        assert!(check_filename("shot 01 ü.mov").is_some());
        for bad in ["../a.mov", "a/b.mov", "a\\b.mov", "", ".hidden", "CON.mov", "a.mov.", "C:x"] {
            assert!(check_filename(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn media_urls_use_the_media_list() {
        assert!(media_url("https://delivery.example.test/v.mov", "input.url").is_ok());
        assert!(media_url("https://api.example.test/ext/manifests/x", "input.url").is_err());
    }
}
