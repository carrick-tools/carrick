//! A service's type-reach record (carrick#3101): the import graph of the
//! program that typed it on main, and the files each manifest alias was
//! asked about in. A PR scan reads main's record to re-type only what its
//! change can reach (carrick#3102).
//!
//! The record is its own gzipped object beside the index blob, never inside
//! it: every blob reader loads whole blobs, and a record runs to megabytes.
//! The blob names it with [`TypeReachRef`], set by the store only after the
//! object is written and checked, so a blob never names a record its store
//! does not hold. The wire is pinned in the Step 0 comment on carrick#3101
//! and in carrick-cloud `docs/internal/reference/payload-staging-protocol.md`.

use super::ScannerBuild;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

/// The record's own format. A PR scan reads a record of another version as
/// no record (carrick#3102).
pub const TYPE_REACH_RECORD_VERSION: u32 = 1;

/// The largest gzipped record either side stores. Over it the scanner stores
/// none and warns; the cloud refuses the stamp.
pub const TYPE_REACH_CAP_BYTES: usize = 16 * 1024 * 1024;

/// The stored object's body, before gzip.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TypeReachRecord {
    /// [`TYPE_REACH_RECORD_VERSION`] when written. The graph's own
    /// `version` is the walk's format, which only the sidecar reads.
    pub version: u32,
    /// The scanner build that wrote the record.
    #[serde(default)]
    pub scanner_build: Option<ScannerBuild>,
    /// The sidecar's `reach_graph` answer, verbatim: paths relative to the
    /// repo root.
    pub graph: serde_json::Value,
    /// Each manifest alias the type stage asked about, with the repo-relative
    /// files of every request that carries it, sorted and deduped. Ordered
    /// maps throughout: stored bytes are compared (carrick#1847).
    #[serde(default)]
    pub aliases: BTreeMap<String, Vec<String>>,
}

/// What a blob says about its service's record: the gzipped object's
/// lowercase-hex SHA-256 and its length, as stored.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TypeReachRef {
    pub sha256: String,
    pub bytes: u64,
}

/// A record as it is stored: the gzipped bytes and the reference that names
/// them. Built once, as soon as the graph is read, and carried beside the
/// service's blob to its store.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeReachObject {
    gz: Vec<u8>,
    reference: TypeReachRef,
}

impl TypeReachObject {
    /// Gzip a record that has already passed the upload boundary.
    pub fn encode(record: &serde_json::Value) -> Result<Self, String> {
        let json = serde_json::to_vec(record).map_err(|e| e.to_string())?;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&json).map_err(|e| e.to_string())?;
        let gz = encoder.finish().map_err(|e| e.to_string())?;
        let (sha256, bytes) = crate::analysis_job::digest(&gz);
        Ok(Self {
            gz,
            reference: TypeReachRef {
                sha256,
                bytes: bytes as u64,
            },
        })
    }

    /// The bytes to store.
    pub fn gz(&self) -> &[u8] {
        &self.gz
    }

    /// What the blob names the stored bytes by.
    pub fn reference(&self) -> &TypeReachRef {
        &self.reference
    }
}

/// Read a stored object back, checked against the reference its blob carries.
/// `None` on any mismatch, or when the bytes do not gunzip and parse: the PR
/// side then has no record for the service and re-types it whole.
pub fn decode_type_reach(stored: &[u8], expected: &TypeReachRef) -> Option<TypeReachRecord> {
    let (sha256, bytes) = crate::analysis_job::digest(stored);
    if bytes as u64 != expected.bytes || sha256 != expected.sha256 {
        return None;
    }
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(stored)
        .read_to_end(&mut json)
        .ok()?;
    serde_json::from_slice(&json).ok()
}

/// The id a record is keyed by on read: the blob's service, or its repo when
/// it names none, as every other cross-repo surface keys a service.
pub fn service_id(data: &super::CloudRepoData) -> &str {
    data.service_name.as_deref().unwrap_or(&data.repo_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> TypeReachRecord {
        TypeReachRecord {
            version: TYPE_REACH_RECORD_VERSION,
            scanner_build: Some(ScannerBuild {
                commit: "a".repeat(40),
                dirty: false,
            }),
            graph: serde_json::json!({ "version": 1, "strings": ["src/a.ts"], "files": [] }),
            aliases: BTreeMap::from([(
                "GET /orders Response".to_string(),
                vec!["src/a.ts".to_string()],
            )]),
        }
    }

    #[test]
    fn a_record_round_trips_through_its_stored_bytes() {
        let record = record();
        let object = TypeReachObject::encode(&serde_json::to_value(&record).unwrap()).unwrap();
        assert_eq!(object.reference().bytes, object.gz().len() as u64);
        assert_eq!(object.reference().sha256.len(), 64);
        assert_eq!(
            decode_type_reach(object.gz(), object.reference()),
            Some(record)
        );
    }

    #[test]
    fn a_record_whose_digest_or_length_disagrees_reads_as_none() {
        let object = TypeReachObject::encode(&serde_json::to_value(record()).unwrap()).unwrap();
        let mut wrong_digest = object.reference().clone();
        wrong_digest.sha256 = "0".repeat(64);
        assert_eq!(decode_type_reach(object.gz(), &wrong_digest), None);
        let mut wrong_length = object.reference().clone();
        wrong_length.bytes += 1;
        assert_eq!(decode_type_reach(object.gz(), &wrong_length), None);
        let truncated = &object.gz()[..object.gz().len() - 1];
        let (sha256, bytes) = crate::analysis_job::digest(truncated);
        let names_the_truncation = TypeReachRef {
            sha256,
            bytes: bytes as u64,
        };
        assert_eq!(decode_type_reach(truncated, &names_the_truncation), None);
    }

    #[test]
    fn the_reference_round_trips_and_writes_the_pinned_names() {
        let reference = TypeReachRef {
            sha256: "ab".repeat(32),
            bytes: 912,
        };
        let json = serde_json::to_value(&reference).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "sha256": "ab".repeat(32), "bytes": 912 })
        );
        assert_eq!(
            serde_json::from_value::<TypeReachRef>(json).unwrap(),
            reference
        );
        let json = serde_json::to_value(record()).unwrap();
        for key in ["version", "scanner_build", "graph", "aliases"] {
            assert!(json.get(key).is_some(), "{key} missing from {json}");
        }
    }
}
