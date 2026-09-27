//! The manifest envelope every op shares (see docs/api.md §2).
//!
//! Only `version`, `op`, `job_id` and `title` belong to the envelope. Everything else in the JSON
//! is the op's own, and the op parses it from the same document (`ops::prepare`).

use futures_util::StreamExt;
use serde::de::DeserializeOwned;
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
    /// The server refused the manifest link; the status says why, so a mix-up is traceable.
    #[error("{}", refused("request link", *.0))]
    Refused(u16),
    #[error("could not fetch the request: {0}")]
    Fetch(String),
    #[error("the request is not valid: {0}")]
    Invalid(String),
    #[error("this request needs a newer version of Ranomaly Local ({0})")]
    Unsupported(String),
}

/// Wording for a link the server refused. Says what the status means and no more: a 404 is
/// as likely a wrong link as an expired one.
pub fn refused(what: &str, status: u16) -> String {
    let why = match status {
        410 => "has expired",
        404 => "was not found — it may have expired, been used already, or be wrong",
        _ => "was refused — it may have expired",
    };
    format!("the {what} {why} (HTTP {status}). Start the job again")
}

impl ManifestError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}

/// Parses the envelope and hands back the whole document for the op to parse.
pub fn parse(bytes: &[u8]) -> Result<(Envelope, serde_json::Value), ManifestError> {
    let doc: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ManifestError::invalid("it is not JSON"))?;
    let env: Envelope = fields(&doc)?;
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
        s @ (401 | 403 | 404 | 410) => return Err(ManifestError::Refused(s)),
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

/// Reads typed fields out of the manifest. Errors name the field in the manifest's own
/// terms ("missing params.width", "input.size must be a whole number"), never parser jargon.
pub fn fields<T: DeserializeOwned>(doc: &serde_json::Value) -> Result<T, ManifestError> {
    serde_path_to_error::deserialize(doc).map_err(|e| {
        let path = e.path().to_string();
        ManifestError::Invalid(describe(&path, &e.into_inner().to_string()))
    })
}

fn describe(path: &str, msg: &str) -> String {
    let msg = msg.split(" at line ").next().unwrap_or(msg);
    let at = |field: &str| if path == "." { field.to_string() } else { format!("{path}.{field}") };
    if let Some(field) = msg.strip_prefix("missing field `").and_then(|r| r.split('`').next()) {
        return format!("missing {}", at(field));
    }
    let expected = msg.rsplit("expected ").next().unwrap_or("");
    let kind = if msg.contains("expected ") {
        match expected {
            e if e.starts_with('u') || e.starts_with('i') => "a whole number",
            e if e.starts_with('f') => "a number",
            "a string" => "text",
            "a boolean" => "true or false",
            e if e.starts_with("a sequence") => "a list",
            e if e.starts_with("struct") || e.starts_with("a map") => "an object",
            _ => expected,
        }
    } else {
        ""
    };
    match (path, kind) {
        (".", "") => msg.to_string(),
        (".", k) => format!("expected {k}"),
        (p, "") => format!("{p}: {msg}"),
        (p, k) => format!("{p} must be {k}"),
    }
}

/// A job input URL: https (see `link::check_url`).
pub fn media_url(raw: &str, field: &str) -> Result<Url, ManifestError> {
    let url = Url::parse(raw).map_err(|_| ManifestError::invalid(format!("{field} is not a URL")))?;
    link::check_url(&url).map_err(|e| ManifestError::invalid(format!("{field}: {e}")))?;
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
        let msg = |b: &[u8]| parse(b).unwrap_err().to_string();
        assert_eq!(msg(b"not json"), "the request is not valid: it is not JSON");
        assert_eq!(msg(br#"{"version":1}"#), "the request is not valid: missing op");
        assert_eq!(msg(br#"{"version":"1","op":"x"}"#), "the request is not valid: version must be a whole number");
    }

    #[test]
    fn refusals_say_what_the_status_means() {
        assert_eq!(ManifestError::Refused(404).to_string(),
            "the request link was not found — it may have expired, been used already, or be wrong (HTTP 404). Start the job again");
        assert!(ManifestError::Refused(403).to_string().contains("was refused"));
        assert!(ManifestError::Refused(410).to_string().contains("has expired"));
    }

    #[test]
    fn field_errors_name_the_field() {
        #[derive(Deserialize, Debug)]
        #[allow(dead_code)]
        struct Op { input: Input }
        #[derive(Deserialize, Debug)]
        #[allow(dead_code)]
        struct Input { size: u64, name: String, list: Vec<u8> }
        let err = |v: serde_json::Value| fields::<Op>(&v).unwrap_err().to_string();
        let tail = |s: String| s.trim_start_matches("the request is not valid: ").to_string();
        assert_eq!(tail(err(serde_json::json!({}))), "missing input");
        assert_eq!(tail(err(serde_json::json!({"input": {"name": "a", "list": []}}))), "missing input.size");
        assert_eq!(tail(err(serde_json::json!({"input": {"size": "big", "name": "a", "list": []}}))), "input.size must be a whole number");
        assert_eq!(tail(err(serde_json::json!({"input": {"size": 1, "name": 5, "list": []}}))), "input.name must be text");
        assert_eq!(tail(err(serde_json::json!({"input": {"size": 1, "name": "a", "list": 3}}))), "input.list must be a list");
    }

    #[test]
    fn filenames() {
        assert!(check_filename("shot 01 ü.mov").is_some());
        for bad in ["../a.mov", "a/b.mov", "a\\b.mov", "", ".hidden", "CON.mov", "a.mov.", "C:x"] {
            assert!(check_filename(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn media_urls_must_be_https() {
        assert!(media_url("https://delivery.example.test/v.mov", "input.url").is_ok());
        assert!(media_url("http://delivery.example.test/v.mov", "input.url").is_err());
    }
}
