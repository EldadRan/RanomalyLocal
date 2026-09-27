//! Compiled-in trust settings. Changing these requires a new build of the helper.
//!
//! The helper fetches from nowhere except these origins. An entry matches a URL when the
//! scheme is https, the host is equal (no subdomains), there is no port or user-info, and the
//! path starts with `path_prefix`. Prefixes end in `/` so `/aab-temp/` never matches
//! `/aab-temp-other/`.

pub struct Origin {
    pub host: &'static str,
    pub path_prefix: &'static str,
}

/// Where a `ranomalyext://` link may point: the manifest.
/// TODO: fill in before the first release (see handoffs/HANDOFF-AAB-video-to-png-2026-09-27.md §1).
#[cfg(not(test))]
pub const MANIFEST_ORIGINS: &[Origin] = &[
    Origin { host: "replace-with-r2-account-id.r2.cloudflarestorage.com", path_prefix: "/aab-temp/" },
];

/// Where a manifest may send the helper for media and other job inputs.
/// TODO: fill in before the first release (CF's delivery host for #9 tickets).
#[cfg(not(test))]
pub const MEDIA_ORIGINS: &[Origin] = &[
    Origin { host: "replace-with-r2-account-id.r2.cloudflarestorage.com", path_prefix: "/aab-media/" },
];

#[cfg(test)]
pub const MANIFEST_ORIGINS: &[Origin] = &[
    Origin { host: "api.example.test", path_prefix: "/ext/manifests/" },
    Origin { host: "acct.r2.cloudflarestorage.com", path_prefix: "/aab-temp/" },
];

#[cfg(test)]
pub const MEDIA_ORIGINS: &[Origin] = &[
    Origin { host: "delivery.example.test", path_prefix: "/" },
    Origin { host: "acct.r2.cloudflarestorage.com", path_prefix: "/aab-media/" },
];

pub const MAX_LINK_BYTES: usize = 8 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
