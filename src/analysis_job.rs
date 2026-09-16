//! The analysis job: the prompts a scan built, shipped as one object, and the
//! answers that come back (carrick#1229).
//!
//! A first index of a large monorepo makes thousands of model calls, and today
//! every one of them has to survive on the laptop that started the scan. A
//! shell that closes takes the run with it. So the scan can stop after the
//! deterministic layer, ship every prompt it built as one **job bundle**, and
//! exit; the cloud answers them on its own time; and any machine that holds the
//! tree later replays the **answer bundle** through the ordinary scan.
//!
//! Three properties make that a replay rather than a second implementation.
//!
//! * **The body is the identity.** A row's `id` is the hex sha256 of the prompt
//!   bytes AFTER the framework-guidance prefix — the same slice the cloud's
//!   analysis cache hashes for its own key, so `scanner id == cloud bodySha`
//!   for the same bytes. It is deliberately not the whole message: the prefix
//!   is not key material on either side, and keying on it would make a release
//!   that reworded the guidance renderer reject every answer the cache would
//!   have served. [`body_id`] is that hash, and one test pins it.
//! * **The join is content, not paths or commits.** A resume rebuilds each
//!   file's body locally — cheap, no model — and takes the answer only when the
//!   hashes agree. So a resume works on a dirty tree, in a shallow clone, at a
//!   different commit and on a different machine; files whose content moved go
//!   to the model like any other changed file.
//! * **The bundle carries only what varies per file.** The rendered guidance
//!   block and the response schema are carried once each in the header and
//!   re-attached by the driver, which is most of the bytes on a repo with
//!   thousands of files.
//!
//! The object is newline-delimited JSON, gzipped. Compression is a requirement
//! rather than an economy: the raw stream on a large monorepo is hundreds of
//! megabytes against a hard per-object ceiling.
//!
//! Reference: `docs/dispatch-resume.md`.

use std::collections::BTreeMap;
use std::io::Write;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The tag the job bundle is written under, and the one the cloud reads.
pub const JOB_SCHEMA: &str = "carrick.analysis-job/0";

/// The tag the answer bundle must carry.
pub const ANSWERS_SCHEMA: &str = "carrick.analysis-answers/0";

/// Name the prompt body the way both sides name it.
///
/// Hex sha256 of the body bytes — everything after the guidance prefix, and
/// nothing else. The cloud computes
/// `sha256(utf8Bytes(userMessage).subarray(guidancePrefixBytes))` for its cache
/// key; this is the same digest over the same bytes, so a row's `id` is the
/// name the answer already has on the other side.
pub fn body_id(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Hex sha256 of a serialized value, for the header's schema map. A bundle
/// key, not a claim about the cloud's own canonicalisation of the schema.
pub fn value_sha(value: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_string(value).unwrap_or_default().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// How many prompts of each kind this job carries.
///
/// `intent_*` are zero in this release: intents are ~1/18th of a scan's calls
/// once batched, and they run on the machine that resumes. The counts stay on
/// the wire because the driver reads them to size the job.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct JobCounts {
    pub analyze_file: usize,
    pub intent_functions: usize,
    pub intent_levels: usize,
}

/// The bundle's first line: everything that is true of the whole job, plus the
/// two constants every row would otherwise repeat.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobHeader {
    pub schema: String,
    pub scan_id: String,
    pub repo: String,
    pub commit: String,
    pub scanner_version: String,
    pub cache_version: u32,
    pub services: Vec<String>,
    /// `guidance_key` → the rendered guidance block, **verbatim**.
    ///
    /// Verbatim rather than by reference: the cloud's guidance store is keyed
    /// server-side on a different space entirely, and the block is rendered in
    /// Rust from five answers, so referencing it would mean porting that
    /// renderer to another language on a seam where drift is silent. The
    /// driver prepends what is here and sets its own prefix length; the cache
    /// key does not read the prefix, so any length is correct by construction.
    pub guidance: BTreeMap<String, String>,
    /// `schema_sha` → the response schema value, carried once.
    pub schemas: BTreeMap<String, serde_json::Value>,
    pub counts: JobCounts,
}

/// One analyze-file prompt: the bytes that vary per file, and the two keys
/// that say what to put in front of them.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AnalyzeRow {
    /// `sha256(body)`, hex. The name the answer comes back under.
    pub id: String,
    pub service: String,
    pub guidance_key: String,
    /// The prompt after the guidance prefix, byte-exact. The file's path is
    /// inside these bytes and is repo-relative (carrick#1223), which is what
    /// makes a bundle resumable on another machine.
    pub body: String,
    pub schema_sha: String,
}

/// A job bundle, assembled in memory and written once.
#[derive(Debug, Clone)]
pub struct JobBundle {
    pub header: JobHeader,
    pub analyze: Vec<AnalyzeRow>,
}

impl JobBundle {
    /// The bytes that go over the wire: newline-delimited JSON, gzipped.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let header = serde_json::to_string(&self.header).map_err(|e| format!("job header: {e}"))?;
        encoder
            .write_all(header.as_bytes())
            .and_then(|()| encoder.write_all(b"\n"))
            .map_err(|e| format!("job bundle: {e}"))?;
        for row in &self.analyze {
            let line = serde_json::to_string(&JobLine::Analyze(row))
                .map_err(|e| format!("job row: {e}"))?;
            encoder
                .write_all(line.as_bytes())
                .and_then(|()| encoder.write_all(b"\n"))
                .map_err(|e| format!("job bundle: {e}"))?;
        }
        encoder.finish().map_err(|e| format!("job bundle: {e}"))
    }
}

/// How a row names itself on the wire, so a reader can tell an analyze row
/// from an intent row without positional rules.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JobLine<'a> {
    Analyze(&'a AnalyzeRow),
}

/// One answer, as the cloud hands it back.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct Answer {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub cached: bool,
    /// The model stopped at its token ceiling. Never replayed: a truncated
    /// answer joined onto rows is a file half-analysed for as long as the
    /// index lives (carrick#692), and the resume can simply ask again.
    #[serde(default)]
    pub truncated: bool,
}

/// The answers a job produced, keyed the way the resume asks for them.
#[derive(Debug, Clone, Default)]
pub struct AnswerBundle {
    answers: BTreeMap<String, Answer>,
    /// Rows the job could not answer, by id and code. Counted, never replayed.
    failures: BTreeMap<String, String>,
    /// Whether the cloud says every row was answered. A partial job is still
    /// worth replaying — every answer in it is one the resume does not buy.
    pub complete: bool,
}

impl AnswerBundle {
    /// Read an answer bundle, gzipped or plain.
    ///
    /// Tolerant by design: a line this scanner does not understand is skipped
    /// rather than failing the resume, because the alternative is discarding
    /// thousands of answers over one unknown row.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let text = decompress(bytes)?;
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        let header: AnswerHeader =
            lines
                .next()
                .ok_or("the answer bundle is empty")
                .and_then(|line| {
                    serde_json::from_str(line).map_err(|_| "the answer bundle has no header")
                })?;
        if header.schema != ANSWERS_SCHEMA {
            return Err(format!(
                "the answer bundle is written as '{}'; this scanner reads {ANSWERS_SCHEMA}. \
                 Update carrick to collect it.",
                header.schema
            ));
        }
        let mut bundle = Self {
            complete: header.complete,
            ..Self::default()
        };
        for line in lines {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            match value.get("kind").and_then(|kind| kind.as_str()) {
                Some("failure") => {
                    let id = value
                        .get("id")
                        .and_then(|id| id.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let code = value
                        .get("code")
                        .and_then(|code| code.as_str())
                        .unwrap_or("no code")
                        .to_string();
                    if !id.is_empty() {
                        bundle.failures.insert(id, code);
                    }
                }
                _ => {
                    let Ok(answer) = serde_json::from_value::<Answer>(value) else {
                        continue;
                    };
                    if !answer.id.is_empty() {
                        bundle.answers.insert(answer.id.clone(), answer);
                    }
                }
            }
        }
        Ok(bundle)
    }

    /// The answer for a body, when there is one worth replaying.
    ///
    /// A truncated answer is not one: it is half a file's analysis, and the
    /// resume can ask the model for the rest of it in the ordinary way.
    pub fn text_for(&self, id: &str) -> Option<&str> {
        self.answers
            .get(id)
            .filter(|answer| !answer.truncated)
            .map(|answer| answer.text.as_str())
    }

    pub fn len(&self) -> usize {
        self.answers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.answers.is_empty()
    }

    pub fn failure_count(&self) -> usize {
        self.failures.len()
    }
}

#[derive(Deserialize)]
struct AnswerHeader {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    complete: bool,
}

/// Gzipped or not: the cloud says which by the bytes, and a reader that
/// insisted would fail a whole resume over a codec.
fn decompress(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(bytes), &mut text)
            .map_err(|e| format!("could not read the answer bundle: {e}"))?;
        return Ok(text);
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| "the answer bundle is not text".to_string())
}

/// What a dispatched scan did, as it crosses from the scan subprocess to the
/// command that started it.
///
/// `job_id` is absent when the run found nothing for the model to answer. That
/// is not a failure and not a job: the repo can be indexed here and now, in
/// the seconds it takes to state facts nobody has to be asked about, and the
/// command that reads this line does exactly that.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Dispatched {
    pub repo: String,
    pub commit: String,
    pub job_id: Option<String>,
    pub analyze_rows: usize,
    pub eta_seconds: Option<u64>,
}

/// Hex sha256 and byte length of an object, for the integrity check the cloud
/// performs on what it was told to expect.
pub fn digest(bytes: &[u8]) -> (String, usize) {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    (format!("{:x}", hasher.finalize()), bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-repo invariant: `scanner id == cloud bodySha`.
    ///
    /// The expected digest was computed over the same bytes the cloud's
    /// `analysis_cache.js` hashes for a `guidance`-mode key —
    /// `sha256(utf8Bytes(message).subarray(prefix_bytes))` — and is written
    /// here as a constant so a change on either side has to move a number
    /// somebody reads. The same triple belongs in the cloud's own test.
    #[test]
    fn the_row_id_is_the_body_after_the_guidance_prefix() {
        let guidance = "## FRAMEWORK GUIDANCE\nexpress routes\n";
        let body = "### FILE CONTENT\nconst a = 1;\n";
        let message = format!("{guidance}{body}");
        let prefix_bytes = guidance.len();

        assert_eq!(prefix_bytes, 37, "the fixture the cloud's test shares");
        assert_eq!(message.len(), 67);
        assert_eq!(
            body_id(&message[prefix_bytes..]),
            "c3856653017da3866d41a3bad561fe22f902cc6fe9b8a79e2b283334eae2655a",
            "the id names the body bytes; if this moved, the cloud's bodySha \
             moved with it or the two have diverged"
        );
    }

    /// The prefix is not key material, on either side. A release that rewords
    /// the guidance renderer must not orphan a dispatched job.
    #[test]
    fn a_different_guidance_block_leaves_the_id_alone() {
        let body = "### FILE CONTENT\nconst a = 1;\n";
        let one = format!("{}{body}", "## GUIDANCE\nshort\n");
        let other = format!("{}{body}", "## GUIDANCE\nquite a lot longer, reworded\n");
        let first = one.len() - body.len();
        let second = other.len() - body.len();
        assert_eq!(body_id(&one[first..]), body_id(&other[second..]));
    }

    /// One byte of the body is a different question and must be a different
    /// name, or a resume would answer a prompt with somebody else's answer.
    #[test]
    fn one_byte_of_the_body_changes_the_id() {
        assert_ne!(body_id("const a = 1;"), body_id("const a = 2;"));
    }

    /// A multi-byte prefix is counted in BYTES, not characters: the cloud
    /// slices a byte array, and a character offset would cut a different
    /// place in a file whose guidance names a non-ASCII framework.
    #[test]
    fn the_prefix_is_counted_in_bytes() {
        let guidance = "## GUIDANCE — ✓\n";
        let body = "### FILE CONTENT\n";
        let message = format!("{guidance}{body}");
        assert_eq!(&message.as_bytes()[guidance.len()..], body.as_bytes());
        assert_eq!(body_id(&message[guidance.len()..]), body_id(body));
    }

    #[test]
    fn a_bundle_round_trips_through_its_own_encoding() {
        let bundle = JobBundle {
            header: JobHeader {
                schema: JOB_SCHEMA.to_string(),
                scan_id: "scan".to_string(),
                repo: "owner/repo".to_string(),
                commit: "abc".to_string(),
                scanner_version: "0.0.0".to_string(),
                cache_version: 1,
                services: vec!["api".to_string()],
                guidance: BTreeMap::from([("k".to_string(), "block".to_string())]),
                schemas: BTreeMap::from([("s".to_string(), serde_json::json!({"type":"object"}))]),
                counts: JobCounts {
                    analyze_file: 1,
                    ..JobCounts::default()
                },
            },
            analyze: vec![AnalyzeRow {
                id: body_id("body"),
                service: "api".to_string(),
                guidance_key: "k".to_string(),
                body: "body".to_string(),
                schema_sha: "s".to_string(),
            }],
        };
        let bytes = bundle.encode().unwrap();
        let text = decompress(&bytes).unwrap();
        let mut lines = text.lines();
        let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["schema"], JOB_SCHEMA);
        assert_eq!(header["counts"]["analyze_file"], 1);
        let row: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(row["kind"], "analyze");
        assert_eq!(row["body"], "body");
        assert!(lines.next().is_none());
        let (sha, size) = digest(&bytes);
        assert_eq!(sha.len(), 64);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn answers_are_read_back_by_body_id_and_a_truncated_one_is_not_replayed() {
        let text = format!(
            "{}\n{}\n{}\n{}\n",
            serde_json::json!({"schema": ANSWERS_SCHEMA, "complete": true}),
            serde_json::json!({"id": "a", "text": "{}", "cached": true}),
            serde_json::json!({"id": "b", "text": "{half", "truncated": true}),
            serde_json::json!({"kind": "failure", "id": "c", "code": "model_refused"}),
        );
        let bundle = AnswerBundle::decode(text.as_bytes()).unwrap();
        assert_eq!(bundle.text_for("a"), Some("{}"));
        assert_eq!(bundle.text_for("b"), None, "truncated answers are misses");
        assert_eq!(bundle.text_for("c"), None);
        assert_eq!(bundle.failure_count(), 1);
        assert!(bundle.complete);
    }

    #[test]
    fn an_answer_bundle_from_a_newer_cloud_is_refused_by_name() {
        let text = format!(
            "{}\n",
            serde_json::json!({"schema": "carrick.analysis-answers/9"})
        );
        let error = AnswerBundle::decode(text.as_bytes()).unwrap_err();
        assert!(error.contains("carrick.analysis-answers/9"), "{error}");
    }

    #[test]
    fn a_gzipped_answer_bundle_reads_the_same_as_a_plain_one() {
        let text = format!(
            "{}\n{}\n",
            serde_json::json!({"schema": ANSWERS_SCHEMA, "complete": false}),
            serde_json::json!({"id": "a", "text": "answer"}),
        );
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        let bundle = AnswerBundle::decode(&encoder.finish().unwrap()).unwrap();
        assert_eq!(bundle.text_for("a"), Some("answer"));
        assert!(!bundle.complete);
        assert_eq!(bundle.len(), 1);
    }
}
