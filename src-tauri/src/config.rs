//! Compiled-in limits.
//!
//! There is deliberately no host allowlist: the helper runs whatever a well-formed
//! `ranomalyext://` link points at, over https. The setup screen shows the source host so the
//! user sees where a job came from before starting it.

pub const MAX_LINK_BYTES: usize = 8 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
