//! Compiled-in trust settings. Changing these requires a new build of the helper.

/// Cloudflare account id; presigned R2 URLs use `<account>.r2.cloudflarestorage.com`.
/// TODO: replace with the real account id before the first release.
pub const R2_ACCOUNT_ID: &str = "REPLACE_WITH_R2_ACCOUNT_ID";

/// Buckets the helper may read manifests and media from.
/// TODO: replace with the real bucket names (temp manifest bucket + media bucket).
pub const R2_BUCKETS: &[&str] = &["aab-temp", "aab-media"];

pub const MAX_LINK_BYTES: usize = 8 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

pub fn r2_host() -> String {
    format!("{}.r2.cloudflarestorage.com", R2_ACCOUNT_ID.to_ascii_lowercase())
}
