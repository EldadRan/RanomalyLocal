//! Strict parsing of `ranomalyext://` links, and the https rule for every URL.
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
    #[error("the link must point to an https address")]
    NotHttps,
}

/// Parses `ranomalyext://run?manifest=<encoded url>` and returns the manifest URL.
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
    check_url(&url)?;
    Ok(url)
}

/// Any https URL without embedded credentials. Debug builds also take http://127.0.0.1 for the
/// local mock servers. There is no host allowlist (see `config`).
pub fn check_url(url: &Url) -> Result<(), LinkError> {
    if !url.username().is_empty() || url.password().is_some() || url.host_str().is_none() {
        return Err(LinkError::Malformed);
    }
    let local_dev = cfg!(debug_assertions) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if url.scheme() != "https" && !local_dev {
        return Err(LinkError::NotHttps);
    }
    Ok(())
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

    fn link_for(target: &str) -> String {
        let enc: String = url::form_urlencoded::byte_serialize(target.as_bytes()).collect();
        format!("ranomalyext://run?manifest={enc}")
    }

    fn ok(u: &str) -> bool {
        check_url(&Url::parse(u).unwrap()).is_ok()
    }

    #[test]
    fn accepts_any_https_url() {
        let target = "https://delivery.example.test/t/abc?sig=1";
        assert_eq!(parse_link(&link_for(target)).unwrap().as_str(), target);
        assert!(ok("https://anything.example/any/path.json"));
        assert!(ok("https://host:8443/x"));
    }

    #[test]
    fn rejects_plain_http_and_credentials() {
        assert_eq!(check_url(&Url::parse("http://example.test/x").unwrap()), Err(LinkError::NotHttps));
        assert!(!ok("ftp://example.test/x"));
        assert!(!ok("file:///etc/passwd"));
        assert!(!ok("https://u:p@example.test/x"));
        assert!(parse_link(&link_for("http://example.test/m.json")).is_err());
    }

    #[test]
    fn debug_builds_accept_the_local_mock() {
        assert!(ok("http://127.0.0.1:8765/aab-temp/m.json"));
        assert!(!ok("http://localhost:8765/aab-temp/m.json"));
    }

    #[test]
    fn rejects_malformed_links() {
        let good = "https://delivery.example.test/m";
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
