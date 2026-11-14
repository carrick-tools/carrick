//! Where a manifest entry's type is declared (carrick#2148, H2).
//!
//! The stub of the failing-first commit: it stores nothing.

use crate::cloud_storage::TypeManifestEntry;
use crate::services::type_sidecar::TypeResolutionResult;

#[allow(dead_code)]
pub fn store_type_homes(
    _manifest: &mut [TypeManifestEntry],
    _resolution: &TypeResolutionResult,
    _repo_path: &str,
) {
}
