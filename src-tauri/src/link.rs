//! Strict parsing of `ranomalyext://` links and the R2 allowlist.
//!
//! Any web page can open a `ranomalyext://` link, so nothing in the link is trusted except
//! a URL that points into one of our own R2 buckets.

use url::Url;

use crate::config;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LinkError {
    #[error("the link is too long")]
    TooLong,
    #[error("the link is not a valid Ranomaly link")]
    Malformed,
    #[error("the link points outside AAB storage")]
    NotAllowed,
}

/// Parses `ranomalyext://run?manifest=<encoded url>` and returns the allowlisted manifest URL.
pub fn parse_link(raw: &str) -> Result<Url, LinkError> {
    if raw.len() > config::MAX_LINK_BYTES {
        return Err(LinkError::TooLong);
    }
    let link = Url::parse(raw).map_err(|_| LinkError::Malformed)?;
    if link.scheme() != "ranomalyext"
        || link.host_str() != Some("run")
        || !(link.path().is_empty() || link.path() == "/")
        || link.fragment().is_some()
        || link.port().is_some()
        || !link.username().is_empty()
        || link.password().is_some()
    {
        return Err(LinkError::Malformed);
    }
    let mut pairs = link.query_pairs();
    let manifest = match (pairs.next(), pairs.next()) {
        (Some((k, v)), None) if k == "manifest" => v.into_owned(),
        _ => return Err(LinkError::Malformed),
    };
    let url = Url::parse(&manifest).map_err(|_| LinkError::Malformed)?;
    check_allowed(&url, config::MANIFEST_ORIGINS)?;
    Ok(url)
}

/// Accepts a URL only if it matches one of `origins` (see `config`).
pub fn check_allowed(url: &Url, origins: &[config::Origin]) -> Result<(), LinkError> {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(LinkError::NotAllowed);
    }
    let path = url.path();
    let prefix_ok = |o: &config::Origin| path.starts_with(o.path_prefix);
    // Debug builds only: the local mock server, on any port, under an allowlisted path.
    if cfg!(debug_assertions)
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && origins.iter().any(prefix_ok)
    {
        return Ok(());
    }
    if url.scheme() != "https" || url.port().is_some() {
        return Err(LinkError::NotAllowed);
    }
    let host = url.host_str().ok_or(LinkError::NotAllowed)?.to_ascii_lowercase();
    if origins.iter().any(|o| o.host.eq_ignore_ascii_case(&host) && prefix_ok(o)) {
        Ok(())
    } else {
        Err(LinkError::NotAllowed)
    }
}

/// Unix time at which a SigV4-presigned URL stops working, if it says so.
pub fn presigned_expiry(url: &Url) -> Option<u64> {
    let mut date = None;
    let mut expires = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "X-Amz-Date" => date = parse_amz_date(&v),
            "X-Amz-Expires" => expires = v.parse::<u64>().ok(),
            _ => {}
        }
    }
    Some(date? + expires?)
}

/// `20260927T101500Z` → unix seconds.
fn parse_amz_date(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(4..6)?, num(6..8)?);
    let (h, mi, se) = (num(9..11)?, num(11..13)?, num(13..15)?);
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + se).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::{MANIFEST_ORIGINS as M, MEDIA_ORIGINS as MEDIA};

    fn link_for(target: &str) -> String {
        let enc: String = url::form_urlencoded::byte_serialize(target.as_bytes()).collect();
        format!("ranomalyext://run?manifest={enc}")
    }

    fn allowed(u: &str, origins: &[config::Origin]) -> bool {
        check_allowed(&Url::parse(u).unwrap(), origins).is_ok()
    }

    #[test]
    fn accepts_listed_origins() {
        let target = "https://acct.r2.cloudflarestorage.com/aab-temp/m/abc.json?X-Amz-Expires=600";
        assert_eq!(parse_link(&link_for(target)).unwrap().as_str(), target);
        assert!(allowed("https://API.example.test/ext/manifests/0f3a", M));
        assert!(allowed("https://delivery.example.test/anything?sig=1", MEDIA));
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "https://acct.r2.cloudflarestorage.com/other-bucket/x.json",
            "https://acct.r2.cloudflarestorage.com/aab-temp-2/x.json",
            "https://acct.r2.cloudflarestorage.com/",
            "http://acct.r2.cloudflarestorage.com/aab-temp/x.json",
            "https://acct.r2.cloudflarestorage.com:8443/aab-temp/x.json",
            "https://evil.example/aab-temp/x.json",
            "https://x.acct.r2.cloudflarestorage.com/aab-temp/x.json",
            "https://acct.r2.cloudflarestorage.com.evil.example/aab-temp/x.json",
            "https://u:p@acct.r2.cloudflarestorage.com/aab-temp/x.json",
            "https://acct.r2.cloudflarestorage.com/aab-temp/../other/x.json",
            // A media origin is not a manifest origin, and the reverse.
            "https://delivery.example.test/x.json",
        ] {
            assert!(!allowed(bad, M), "{bad}");
        }
        assert!(!allowed("https://api.example.test/ext/manifests/x", MEDIA));
    }

    #[test]
    fn debug_builds_accept_the_local_mock_under_listed_paths() {
        assert!(allowed("http://127.0.0.1:8765/aab-temp/m.json", M));
        assert!(!allowed("http://127.0.0.1:8765/elsewhere/m.json", M));
        assert!(!allowed("http://localhost:8765/aab-temp/m.json", M));
    }

    #[test]
    fn rejects_malformed_links() {
        let good = "https://acct.r2.cloudflarestorage.com/aab-temp/x.json";
        let enc: String = url::form_urlencoded::byte_serialize(good.as_bytes()).collect();
        for bad in [
            format!("ranomalyext://other?manifest={enc}"),
            format!("ranomalyext://run/extra?manifest={enc}"),
            format!("ranomalyext://run?manifest={enc}&x=1"),
            format!("ranomalyext://run?manifest={enc}#frag"),
            format!("otherscheme://run?manifest={enc}"),
            "ranomalyext://run".into(),
            "ranomalyext://run?manifest=not%20a%20url".into(),
        ] {
            assert!(parse_link(&bad).is_err(), "{bad}");
        }
        assert_eq!(parse_link(&"x".repeat(9000)), Err(LinkError::TooLong));
    }

    #[test]
    fn reads_presigned_expiry() {
        let url = Url::parse("https://h/b/x?X-Amz-Date=19700101T000100Z&X-Amz-Expires=600").unwrap();
        assert_eq!(presigned_expiry(&url), Some(660));
        let url = Url::parse("https://h/b/x?X-Amz-Date=20260927T101500Z&X-Amz-Expires=0").unwrap();
        assert_eq!(presigned_expiry(&url), Some(1_790_504_100));
    }
}
