//! A file whose answer cannot fit one model response is asked in parts
//! (carrick#1898), and a part shows a window of the file and not the file
//! (carrick#2308).
//!
//! The scanner asks about a file in one request. When the cloud answers that
//! request with its final verdict that the answer was cut (`output_truncated`,
//! not retriable), the file is cut into windows of its lines, each part is
//! asked on its own, and the answers are merged into the one answer the file
//! stores. These tests drive `FileOrchestrator::analyze_files` over a
//! synthetic route table and over generated large files with the model
//! mocked, and every one of them counts the requests the scan sent:
//!
//! - a cut whole answer becomes one stored answer that is the union of its
//!   parts, whatever order the parts are merged in;
//! - a stored whole answer split across the parts by candidate id merges back
//!   to itself exactly;
//! - a part's request holds its window, the declarations the window uses and
//!   nothing else of the file;
//! - a part that is cut is halved, and a single group that is cut loses the
//!   file by name;
//! - nothing else is split: a file that answers, a file lost for any other
//!   reason, and every file of a mocked scan.
//!
//! Its own test binary, `#[serial]`, and every count read as a delta: the
//! request counters, the injected answers and the scan-health registry are
//! process-globals. Each test names its fixture's routes after itself, so an
//! injected answer can only ever match that test's prompts.

use carrick::agent_service::{self, AgentService};
use carrick::agents::file_analyzer_agent::{FileAnalysisResult, FileAnalyzerAgent};
use carrick::agents::file_orchestrator::{
    FILE_TOO_LARGE_CODE, FileCentricAnalysisResult, FileOrchestrator, ListedCandidate,
    MAX_PART_CANDIDATES, PART_CONTEXT_BYTES, PART_WINDOW_BYTES, PartAnswer, WHOLE_PROMPT_MAX_BYTES,
    WRAPPER_SNIPPET_MAX, merge_part_answers,
};
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::agents::source_parts::{Limits, ShownLines, SourcePart, source_parts};
use carrick::framework_detector::DetectionResult;
use carrick::graphql::GraphqlConsumerHints;
use carrick::operation::Protocol;
use serde_json::{Value, json};
use serial_test::serial;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[path = "support/shapes.rs"]
mod shapes;

const ROUTE: &str = "/analyze-file";

/// What a file is cut under when the verdict states no part size.
const LIMITS: Limits = Limits {
    window_bytes: PART_WINDOW_BYTES,
    context_bytes: PART_CONTEXT_BYTES,
    candidates: MAX_PART_CANDIDATES,
};

/// Modules the table imports its handlers from. Each one makes a call the
/// scanner raises and exports a binding, so its source rides in the prompt of
/// a file that imports it.
const HANDLER_MODULES: usize = 4;

/// The table's registrations, in source order:
///
/// - 30 single registrations whose handler is declared in the table itself;
/// - one chained registration of three links, which share a start;
/// - 29 single registrations whose handlers come from all four modules;
/// - 8 single registrations whose handler comes from the first module.
///
/// 70 candidates. At 32 a part, the chain cannot join the first 30 without
/// passing the limit, so the parts hold 30, 32 and 8.
const LOCAL_SINGLES: usize = 30;
const CHAIN_LINKS: usize = 3;
const SINGLES: usize = 67;
const LAST_PART_FIRST_SINGLE: usize = 59;
const CANDIDATES: usize = SINGLES + CHAIN_LINKS;

/// The three parts as ranges over the candidates in the order the prompt
/// lists them, and each part's window: the table's seven lines of imports and
/// declarations and its first 30 registrations, the chain and the 29
/// registrations after it, and the last eight.
const PART_RANGES: [(usize, usize); 3] = [(0, 30), (30, 62), (62, 70)];
const PART_WINDOWS: [(u32, u32); 3] = [(1, 37), (38, 67), (68, 75)];

fn handler_source(n: usize) -> String {
    format!("export function h{n}(req: any, res: any) {{\n  res.send({{ handled: {n} }});\n}}\n")
}

fn single(out: &mut String, tag: &str, i: usize, handler: &str) {
    writeln!(out, "table.get(\"/{tag}/r/{i}\", {handler});").unwrap();
}

fn table_source(tag: &str) -> String {
    let mut out = String::new();
    for n in 0..HANDLER_MODULES {
        writeln!(out, "import {{ h{n} }} from \"./handlers/h{n}\";").unwrap();
    }
    out.push_str("declare function makeTable(): any;\n");
    out.push_str("const table = makeTable();\n");
    out.push_str("const local = (req: any, res: any) => {};\n");
    for i in 0..LOCAL_SINGLES {
        single(&mut out, tag, i, "local");
    }
    writeln!(out, "table.route(\"/{tag}/c/0\").get(h0).post(h1);").unwrap();
    for i in LOCAL_SINGLES..LAST_PART_FIRST_SINGLE {
        single(&mut out, tag, i, &format!("h{}", i % HANDLER_MODULES));
    }
    for i in LAST_PART_FIRST_SINGLE..SINGLES {
        single(&mut out, tag, i, "h0");
    }
    out
}

/// One synthetic service on disk: a table and the modules it imports.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    files: Vec<PathBuf>,
    /// The table's path as `analyze_files` keys it.
    table: String,
    /// The table's path as its prompt names it.
    table_prompt_path: String,
}

impl Fixture {
    /// The 70-candidate table and its four handler modules.
    fn table(tag: &str) -> Self {
        Self::table_from(tag, table_source(tag))
    }

    /// The table written as `source` says, and its four handler modules.
    fn table_from(tag: &str, source: String) -> Self {
        let mut modules: Vec<(String, String)> = (0..HANDLER_MODULES)
            .map(|n| (format!("src/handlers/h{n}.ts"), handler_source(n)))
            .collect();
        modules.push((format!("src/{tag}_table.ts"), source));
        Self::of(&modules, &format!("src/{tag}_table.ts"))
    }

    /// Only the table, with no module to import: the route table of
    /// [`function_table_source`].
    fn lone_table(tag: &str, source: String) -> Self {
        let subject = format!("src/{tag}_table.ts");
        Self::of(&[(subject.clone(), source)], &subject)
    }

    /// `modules` written under a fresh root, with `subject` the file the test
    /// is about.
    fn of(modules: &[(String, String)], subject: &str) -> Self {
        let tmp = tempfile::tempdir().expect("a temp dir");
        // Canonical, as the engine hands the orchestrator its root: the
        // prompt names a file by its path under this.
        let root = tmp.path().canonicalize().expect("a canonical temp dir");
        let mut files = Vec::new();
        for (relative, contents) in modules {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            files.push(path);
        }
        files.sort();
        Self {
            table: root.join(subject).to_string_lossy().to_string(),
            table_prompt_path: subject.to_string(),
            _tmp: tmp,
            root,
            files,
        }
    }

    async fn analyze(&self) -> FileCentricAnalysisResult {
        self.analyze_with(&HashMap::new(), &Default::default())
            .await
    }

    /// A scan with `cached` as the answers a previous scan stored, and
    /// `consumer` as the service's GraphQL consumer hints.
    async fn analyze_with(
        &self,
        cached: &HashMap<String, FileAnalysisResult>,
        consumer: &GraphqlConsumerHints,
    ) -> FileCentricAnalysisResult {
        FileOrchestrator::new(AgentService::new())
            .analyze_files(
                &self.files,
                cached,
                &ProtocolGuidance::from([(Protocol::Http, guidance())]),
                &detection(),
                &self.root,
                &self.root,
                &[],
                &Default::default(),
                consumer,
                &carrick::url_normalizer::UrlNormalizer::default_permissive(),
            )
            .await
            .expect("the analysis runs")
    }

    /// What one scan in which every request answers sent about the subject,
    /// read back from its dump, and how many requests it sent.
    async fn dumped_run(&self) -> (Dumps, usize) {
        let dump = tempfile::tempdir().unwrap();
        dump_into(Some(dump.path()));
        let before = requests();
        self.analyze().await;
        let sent = requests() - before;
        dump_into(None);
        (self.dumps(dump.path()), sent)
    }

    /// What every request about the subject carries and no other file's does.
    fn whole_marker(&self) -> String {
        format!("### FILE CONTENT (Path: {})", self.table_prompt_path)
    }

    /// The subject's whole prompt and every part prompt a run dumped, read
    /// back from `dump`.
    fn dumps(&self, dump: &Path) -> Dumps {
        let stem = self.table_prompt_path.replace('/', "_");
        let mut whole = None;
        let mut parts = BTreeMap::new();
        let mut terms = BTreeMap::new();
        for entry in std::fs::read_dir(dump).expect("the dump dir") {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let Some(rest) = name.strip_prefix(&stem) else {
                continue;
            };
            let payload: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let prompt = payload["request_user_message"]
                .as_str()
                .unwrap()
                .to_string();
            if rest == ".json" {
                assert!(payload.get("part").is_none(), "a whole dump names no part");
                whole = Some(prompt);
            } else if let Some(label) = rest
                .strip_prefix(".part-")
                .and_then(|r| r.strip_suffix(".json"))
            {
                assert_eq!(payload["part"], label, "a part dump names its part");
                parts.insert(label.to_string(), prompt);
                // A dump that states no terms is read as a part of no candidates;
                // a test that needs them says so when it looks one up.
                let Some(ids) = payload["candidate_ids"].as_object() else {
                    continue;
                };
                let ids = ids
                    .iter()
                    .map(|(local, file)| (local.clone(), file.as_str().unwrap().to_string()))
                    .collect();
                let runs: Vec<(u32, u32)> = serde_json::from_value(payload["shown_runs"].clone())
                    .expect("a part dump names the lines it shows");
                terms.insert(
                    label.to_string(),
                    PartTerms {
                        ids,
                        shown: ShownLines {
                            window: runs[0],
                            runs,
                            markers: false,
                        },
                    },
                );
            }
        }
        Dumps {
            whole,
            parts,
            terms,
        }
    }

    /// The subject's whole prompt, from a run in which every file answers.
    async fn whole_prompt(&self) -> String {
        let dump = tempfile::tempdir().unwrap();
        dump_into(Some(dump.path()));
        self.analyze().await;
        dump_into(None);
        self.dumps(dump.path())
            .whole
            .expect("a file that answers whole dumps its prompt")
    }

    /// The subject's candidates, in the order its prompt lists them.
    async fn listed(&self) -> Vec<Listed> {
        listed_in(&self.whole_prompt().await)
    }
}

struct Dumps {
    whole: Option<String>,
    parts: BTreeMap<String, String>,
    /// What each part's dump says of its own terms, by part label.
    terms: BTreeMap<String, PartTerms>,
}

/// What a part's dump states beside its prompt (carrick#2794): the file id
/// each of its local candidate ids stands for, and the file lines it shows.
/// Enough to write the answer a model gives to that prompt, and to say what
/// the scanner should read it as.
#[derive(Debug, Clone)]
struct PartTerms {
    /// (the id the part's prompt gives, the file's id), in the order the part
    /// lists its candidates.
    ids: Vec<(String, String)>,
    shown: ShownLines,
}

impl PartTerms {
    fn file_id(&self, local: &str) -> Option<&str> {
        self.ids
            .iter()
            .find(|(id, _)| id == local)
            .map(|(_, file)| file.as_str())
    }

    fn local_id(&self, file: &str) -> Option<&str> {
        self.ids
            .iter()
            .find(|(_, id)| id == file)
            .map(|(local, _)| local.as_str())
    }
}

/// The part among `terms` that lists the candidate `file_id`.
fn terms_holding<'a>(terms: &'a BTreeMap<String, PartTerms>, file_id: &str) -> &'a PartTerms {
    terms
        .values()
        .find(|part| part.local_id(file_id).is_some())
        .unwrap_or_else(|| panic!("no part lists {file_id}: {terms:?}"))
}

/// The answer a model gives to a part's prompt for the rows of `whole` that
/// part can see: its candidates under the ids the prompt gives them, and the
/// rows with no candidate only when the part shows their line, every line as
/// the prompt numbers it. Rows stay in their order.
fn in_part_terms(whole: &Value, part: &PartTerms) -> Value {
    let renumber = |row: &Value| -> Option<Value> {
        let mut row = row.clone();
        let line = u32::try_from(row["line_number"].as_i64()?).ok()?;
        row["line_number"] = json!(part.shown.prompt_line(line)?);
        Some(row)
    };
    let of_candidates = |name: &str| -> Vec<Value> {
        whole[name]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let local = part.local_id(row["candidate_id"].as_str()?)?;
                let mut row = renumber(row)?;
                row["candidate_id"] = json!(local);
                Some(row)
            })
            .collect()
    };
    let of_lines = |name: &str| -> Vec<Value> {
        whole[name]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(renumber)
            .collect()
    };
    json!({
        "mounts": of_lines("mounts"),
        "endpoints": of_candidates("endpoints"),
        "data_calls": of_candidates("data_calls"),
        "pubsub_operations": of_lines("pubsub_operations"),
    })
}

/// One candidate as a prompt's CANDIDATE CONTEXT section states it.
#[derive(Debug, Clone)]
struct Listed {
    id: String,
    span_start: u32,
    span_end: u32,
    line: i64,
}

/// The candidates a file asked in parts lists, in source order: each part's,
/// parts in the order their labels sort, which is source order. Read back
/// into the file's terms through each part's dump: a part's prompt names its
/// candidates and their lines in its own (carrick#2794).
fn listed_in_parts(dumps: &Dumps) -> Vec<Listed> {
    dumps
        .parts
        .iter()
        .flat_map(|(label, prompt)| {
            let terms = &dumps.terms[label];
            listed_in(prompt).into_iter().map(move |candidate| {
                let id = terms
                    .file_id(&candidate.id)
                    .unwrap_or_else(|| panic!("{label} lists {}", candidate.id));
                let (start, end) = id
                    .strip_prefix("span:")
                    .and_then(|span| span.split_once('-'))
                    .expect("a file id names a span");
                Listed {
                    id: id.to_string(),
                    span_start: start.parse().unwrap(),
                    span_end: end.parse().unwrap(),
                    line: terms
                        .shown
                        .file_line(candidate.line)
                        .expect("a candidate's line is shown") as i64,
                }
            })
        })
        .collect()
}

fn listed_in(prompt: &str) -> Vec<Listed> {
    section(prompt, "CANDIDATE CONTEXT")
        .lines()
        // A part's context is written with its keys sorted, so the id is not
        // the first key (carrick#2794).
        .filter(|line| line.starts_with('{') && line.contains("\"candidate_id\""))
        .map(|line| {
            // The section's closing note follows the last object on its line.
            let value: Value = serde_json::Deserializer::from_str(line)
                .into_iter()
                .next()
                .expect("a candidate context line")
                .expect("a candidate context object");
            Listed {
                id: value["candidate_id"].as_str().unwrap().to_string(),
                // A part's context states no span (carrick#2794).
                span_start: value["span_start"].as_u64().unwrap_or(0) as u32,
                span_end: value["span_end"].as_u64().unwrap_or(0) as u32,
                line: value["line_number"].as_i64().unwrap(),
            }
        })
        .collect()
}

fn as_listed_candidates(listed: &[Listed]) -> Vec<ListedCandidate> {
    listed
        .iter()
        .map(|c| ListedCandidate {
            candidate_id: c.id.clone(),
            span_start: c.span_start,
            span_end: c.span_end,
        })
        .collect()
}

/// The body of the `### <name>` section of a prompt, or nothing when the
/// prompt has no such section.
fn section<'a>(prompt: &'a str, name: &str) -> &'a str {
    let header = format!("\n### {name}");
    let Some(start) = prompt.find(&header) else {
        return "";
    };
    let body = &prompt[start + 1..];
    let body = &body[body.find('\n').map_or(body.len(), |i| i + 1)..];
    &body[..body.find("\n### ").map_or(body.len(), |i| i + 1)]
}

/// A prompt with its per-part sections removed, and what the FILE CONTENT
/// section holds under its header: what a part must hold byte for byte as the
/// whole prompt does.
fn outside_the_part_sections(prompt: &str) -> String {
    let mut out = prompt.to_string();
    let header = out.find("### FILE CONTENT").expect("the header");
    let under = header + out[header..].find('\n').expect("a line under the header");
    let close = out.find("\n```\n\nAnalyze this file").expect("the close");
    out.replace_range(under..close, "");
    for name in [
        "CANDIDATE TARGETS",
        "CANDIDATE CONTEXT",
        "IMPORT TABLE",
        "IMPORTED HTTP WRAPPER DEFINITIONS",
    ] {
        let header = format!("\n### {name}");
        if let Some(start) = out.find(&header) {
            let end = out[start + 1..]
                .find("\n### ")
                .map_or(out.len(), |i| start + 1 + i);
            out.replace_range(start..end, "");
        }
    }
    out
}

/// The modules a prompt's imported-source section holds, in order.
fn attached_modules(prompt: &str) -> Vec<String> {
    section(prompt, "IMPORTED HTTP WRAPPER DEFINITIONS")
        .lines()
        .filter_map(|line| line.strip_prefix("--- wrapper module: "))
        .map(|rest| rest.trim_end_matches(" ---").to_string())
        .collect()
}

fn guidance() -> FrameworkGuidance {
    let pattern = |pattern: &str, description: &str| PatternExample {
        pattern: pattern.to_string(),
        description: description.to_string(),
        framework: "generic".to_string(),
    };
    FrameworkGuidance {
        mount_patterns: vec![],
        endpoint_patterns: vec![pattern(".get(", "a route registration")],
        middleware_patterns: vec![],
        data_fetching_patterns: vec![],
        triage_hints: String::new(),
        parsing_notes: String::new(),
        guidance_key: None,
    }
}

fn detection() -> DetectionResult {
    DetectionResult {
        frameworks: vec![],
        data_fetchers: vec![],
        messaging_clients: vec![],
        socket_clients: vec![],
        notes: String::new(),
        client_semantics: None,
    }
}

fn offline() {
    // SAFETY: every test in this binary is `#[serial]`, so no other thread
    // reads the environment while these are set.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
        std::env::remove_var("CARRICK_EVAL_DUMP_DIR");
    }
    // The registry keeps a service's losses for the life of the process: a
    // loss an earlier test recorded would read to the engine as work this
    // scan still owes.
    carrick::scan_health::forget_service_losses(None);
}

fn dump_into(dir: Option<&Path>) {
    // SAFETY: `#[serial]`, as in `offline`.
    unsafe {
        match dir {
            Some(dir) => std::env::set_var("CARRICK_EVAL_DUMP_DIR", dir),
            None => std::env::remove_var("CARRICK_EVAL_DUMP_DIR"),
        }
    }
}

fn requests() -> usize {
    agent_service::request_counts()
        .get(ROUTE)
        .copied()
        .unwrap_or(0)
}

/// An error body as a prompt lambda sends it. The sentence is this test's
/// own: the scanner reads the code and the flag, never the words.
fn refusal(code: &str, retriable: bool) -> String {
    json!({
        "success": false,
        "error": { "code": code, "message": "stated by the test", "retriable": retriable },
    })
    .to_string()
}

/// The cloud's final verdict that an answer did not fit one response.
fn cut() -> String {
    refusal("output_truncated", false)
}

/// The same verdict, stating the most candidates a part may hold.
fn cut_stating(max_part_candidates: Value) -> String {
    json!({
        "success": false,
        "error": {
            "code": "output_truncated",
            "message": "stated by the test",
            "retriable": false,
            "details": { "max_part_candidates": max_part_candidates },
        },
    })
    .to_string()
}

/// Answer the table's whole request with `verdict` and let every part
/// answer. Returns the requests the scan sent and how many candidates each
/// part listed, parts in source order.
async fn requests_and_part_sizes(fx: &Fixture, verdict: &str) -> (usize, Vec<usize>) {
    let dump = tempfile::tempdir().unwrap();
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, verdict);
    dump_into(Some(dump.path()));
    let before = requests();
    fx.analyze().await;
    dump_into(None);
    let sent = requests() - before;
    let sizes = fx
        .dumps(dump.path())
        .parts
        .values()
        .map(|prompt| listed_in(prompt).len())
        .collect();
    (sent, sizes)
}

/// What every part of the table says of itself, from a run in which each
/// request in `refusals` is refused once as its marker says and every other
/// request is answered. The same refusals then go to the run under test.
async fn part_terms(fx: &Fixture, refusals: &[(String, String)]) -> BTreeMap<String, PartTerms> {
    part_dumps(fx, refusals).await.terms
}

/// [`part_terms`], with the prompts the parts were sent as well.
async fn part_dumps(fx: &Fixture, refusals: &[(String, String)]) -> Dumps {
    let dump = tempfile::tempdir().unwrap();
    for (marker, verdict) in refusals {
        agent_service::inject_mock_envelope(ROUTE, marker, 1, verdict);
    }
    dump_into(Some(dump.path()));
    fx.analyze().await;
    dump_into(None);
    fx.dumps(dump.path())
}

/// What the hint line of single registration `i` holds and nothing else in a
/// request does, as it reads inside the serialized request body.
fn single_marker(tag: &str, i: usize) -> String {
    format!("[path: \\\"/{tag}/r/{i}\\\"]")
}

/// Why this run lost `path`, when it did: owed or for good.
fn lost_reason(path: &str) -> Option<String> {
    carrick::scan_health::unanalysed_files_for(None)
        .into_iter()
        .chain(carrick::scan_health::final_losses_for(None))
        .find(|lost| lost.path == path)
        .map(|lost| lost.reason)
}

fn endpoint_row(candidate: &Listed, position: usize) -> Value {
    json!({
        "candidate_id": candidate.id,
        "line_number": candidate.line,
        "owner_node": "table",
        "method": "GET",
        "path": format!("/row/{position}"),
        "handler_name": "handler",
        "pattern_matched": ".get(",
        "payload_expression_text": null,
        "payload_expression_line": null,
        "response_expression_text": null,
        "response_expression_line": null,
        "primary_type_symbol": null,
        "type_import_source": null,
    })
}

fn data_call_row(candidate: &Listed, position: usize) -> Value {
    json!({
        "candidate_id": candidate.id,
        "line_number": candidate.line,
        "target": format!("/called/{position}"),
        "method": "GET",
        "pattern_matched": "call",
        "payload_expression_text": null,
        "payload_expression_line": null,
        "primary_type_symbol": null,
        "type_import_source": null,
    })
}

fn mount_row(child: &str) -> Value {
    json!({
        "line_number": 6,
        "parent_node": "table",
        "child_node": child,
        "mount_path": format!("/{child}"),
        "import_source": null,
        "pattern_matched": ".use(",
    })
}

fn pubsub_row(topic: &str) -> Value {
    json!({
        "topic": topic,
        "role": "publisher",
        "line_number": 7,
        "primary_type_symbol": null,
        "type_import_source": null,
        "broker": null,
    })
}

/// A whole answer for the table, rows in the order the candidates are listed:
/// a route for two candidates in three (the model is silent about a candidate
/// that is neither a route nor a call), a call at three of them, and three
/// rows that carry no candidate id.
fn whole_answer(listed: &[Listed]) -> Value {
    let endpoints: Vec<Value> = listed
        .iter()
        .enumerate()
        .filter(|(position, _)| position % 3 != 0)
        .map(|(position, candidate)| endpoint_row(candidate, position))
        .collect();
    let data_calls: Vec<Value> = [5, 40, 65]
        .into_iter()
        .map(|position| data_call_row(&listed[position], position))
        .collect();
    json!({
        "mounts": [mount_row("first"), mount_row("second")],
        "endpoints": endpoints,
        "data_calls": data_calls,
        "pubsub_operations": [pubsub_row("table.ready")],
    })
}

/// `whole` as one part would answer it: the rows of the candidates listed at
/// `from..to`, and every row that carries no candidate id, because each part
/// reads the whole file.
fn projected(whole: &Value, listed: &[Listed], (from, to): (usize, usize)) -> Value {
    let ids: Vec<&str> = listed[from..to].iter().map(|c| c.id.as_str()).collect();
    let rows_of = |name: &str| -> Vec<Value> {
        whole[name]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| ids.contains(&row["candidate_id"].as_str().unwrap()))
            .cloned()
            .collect()
    };
    json!({
        "mounts": whole["mounts"],
        "endpoints": rows_of("endpoints"),
        "data_calls": rows_of("data_calls"),
        "pubsub_operations": whole["pubsub_operations"],
    })
}

/// The same answer with its candidate rows written last first: the order a
/// part's rows arrive in is the model's, and the merge must not keep it.
fn reversed(mut answer: Value) -> Value {
    for name in ["endpoints", "data_calls"] {
        answer[name].as_array_mut().unwrap().reverse();
    }
    answer
}

/// An answer as the scanner stores it, for comparing byte for byte.
fn stored_form(answer: &Value) -> String {
    let parsed = FileAnalyzerAgent::result_from_answer("expected", &answer.to_string())
        .expect("the expected answer parses");
    serde_json::to_string(&parsed).unwrap()
}

fn bytes(answer: &FileAnalysisResult) -> String {
    serde_json::to_string(answer).unwrap()
}

/// The first single registration of each of the table's three parts.
fn part_markers(tag: &str) -> [String; 3] {
    [
        single_marker(tag, 0),
        single_marker(tag, LOCAL_SINGLES),
        single_marker(tag, LAST_PART_FIRST_SINGLE),
    ]
}

/// Cut the table's whole answer and answer each of its three parts with its
/// share of `whole`, rows reversed, as the model reads that part's prompt:
/// in the part's candidate ids and the part's line numbers. Returns the same
/// answers in the file's own terms, in part order.
async fn cut_whole_and_answer_parts(
    fx: &Fixture,
    tag: &str,
    whole: &Value,
    listed: &[Listed],
) -> Vec<Value> {
    let terms = part_terms(fx, &[(fx.whole_marker(), cut())]).await;
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    let mut in_file_terms = Vec::new();
    for (marker, range) in part_markers(tag).iter().zip(PART_RANGES) {
        let part = terms_holding(&terms, &listed[range.0].id);
        let answer = reversed(in_part_terms(whole, part));
        agent_service::inject_mock_answer(ROUTE, marker, 1, &answer.to_string());
        in_file_terms.push(reversed(projected(whole, listed, range)));
    }
    in_file_terms
}

/// The fixture is what the tests' expected parts are read off, so its shape
/// is pinned before anything is asserted about parts.
fn assert_the_table_lists_what_the_tests_assume(listed: &[Listed]) {
    assert_eq!(listed.len(), CANDIDATES, "the table's candidates");
    let chain = &listed[LOCAL_SINGLES..LOCAL_SINGLES + CHAIN_LINKS];
    assert!(
        chain.iter().all(|c| c.span_start == chain[0].span_start),
        "the chain's links share a start: {chain:?}"
    );
    let starts: Vec<u32> = listed.iter().map(|c| c.span_start).collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] <= pair[1]),
        "the prompt lists the table's candidates in source order"
    );
    let mut distinct = starts.clone();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        SINGLES + 1,
        "every other candidate starts on its own"
    );
}

#[tokio::test]
#[serial]
async fn a_cut_whole_answer_is_stored_once_as_the_union_of_its_parts() {
    offline();
    let tag = "union";
    let fx = Fixture::table(tag);
    let listed = fx.listed().await;
    assert_the_table_lists_what_the_tests_assume(&listed);

    let whole = whole_answer(&listed);
    cut_whole_and_answer_parts(&fx, tag, &whole, &listed).await;

    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(
        requests() - before,
        HANDLER_MODULES + 1 + 3,
        "one request a handler module, the table's whole request, and its three parts"
    );

    assert_eq!(
        result.raw_model_results.len(),
        HANDLER_MODULES + 1,
        "one stored answer a path"
    );
    let stored = result
        .raw_model_results
        .get(&fx.table)
        .expect("the table has a stored answer");
    assert_eq!(
        bytes(stored),
        stored_form(&whole),
        "the union of the parts' rows, in the candidates' source order"
    );
    assert_eq!(lost_reason(&fx.table), None, "the table is not lost");
    assert_eq!(result.stats.files_analysis_failed, 0);
}

#[tokio::test]
#[serial]
async fn the_merged_answer_is_the_same_bytes_whatever_order_its_parts_arrive_in() {
    offline();
    let tag = "shuffle";
    let fx = Fixture::table(tag);
    let listed = fx.listed().await;
    let whole = whole_answer(&listed);
    let answers = cut_whole_and_answer_parts(&fx, tag, &whole, &listed).await;

    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(requests() - before, HANDLER_MODULES + 1 + 3);
    let stored = bytes(&result.raw_model_results[&fx.table]);

    // The same three answers, handed to the merge in every order.
    let candidates = as_listed_candidates(&listed);
    let parts: Vec<PartAnswer> = PART_WINDOWS
        .iter()
        .zip(&answers)
        .map(|(window, answer)| PartAnswer {
            window: *window,
            answer: FileAnalyzerAgent::result_from_answer("part", &answer.to_string())
                .expect("a part answer parses"),
        })
        .collect();
    let before = requests();
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let arrived: Vec<PartAnswer> = order.iter().map(|part| parts[*part].clone()).collect();
        assert_eq!(
            bytes(&merge_part_answers(&candidates, arrived)),
            stored,
            "parts arriving in the order {order:?}"
        );
    }
    assert_eq!(requests() - before, 0, "merging asks nothing");
}

#[tokio::test]
#[serial]
async fn a_stored_whole_answer_split_by_candidate_id_merges_back_to_itself() {
    offline();
    let tag = "projection";
    let fx = Fixture::table(tag);
    let listed = fx.listed().await;
    let whole = whole_answer(&listed);

    // The answer as one request stores it.
    agent_service::inject_mock_answer(ROUTE, &fx.whole_marker(), 1, &whole.to_string());
    let before = requests();
    let answered_whole = fx.analyze().await;
    assert_eq!(requests() - before, HANDLER_MODULES + 1);
    let stored_whole = bytes(&answered_whole.raw_model_results[&fx.table]);

    // The same rows, each given back by the part that lists its candidate.
    let terms = part_terms(&fx, &[(fx.whole_marker(), cut())]).await;
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    for (marker, range) in part_markers(tag).iter().zip(PART_RANGES) {
        let part = in_part_terms(&whole, terms_holding(&terms, &listed[range.0].id));
        agent_service::inject_mock_answer(ROUTE, marker, 1, &part.to_string());
    }
    let before = requests();
    let answered_in_parts = fx.analyze().await;
    assert_eq!(requests() - before, HANDLER_MODULES + 1 + 3);

    assert_eq!(
        bytes(&answered_in_parts.raw_model_results[&fx.table]),
        stored_whole,
        "the parts merge back to the whole answer exactly"
    );
}

#[tokio::test]
#[serial]
async fn a_part_that_is_cut_is_halved_by_groups() {
    offline();
    let tag = "halved";
    let fx = Fixture::table(tag);
    let listed = fx.listed().await;
    let whole = whole_answer(&listed);

    // The middle part holds 30 groups: the chain and 29 singles. Halved, the
    // front half is the chain and the singles 30..44, the back half 44..59.
    let (from, to) = PART_RANGES[1];
    let front = (from, from + CHAIN_LINKS + 14);
    let back = (front.1, to);
    let refusals = [
        (fx.whole_marker(), cut()),
        (single_marker(tag, LOCAL_SINGLES), cut()),
    ];
    let terms = part_terms(&fx, &refusals).await;
    for (marker, verdict) in &refusals {
        agent_service::inject_mock_envelope(ROUTE, marker, 1, verdict);
    }
    for (marker, range) in [
        (single_marker(tag, 0), PART_RANGES[0]),
        (single_marker(tag, LOCAL_SINGLES), front),
        (single_marker(tag, LOCAL_SINGLES + 14), back),
        (single_marker(tag, LAST_PART_FIRST_SINGLE), PART_RANGES[2]),
    ] {
        let part = reversed(in_part_terms(
            &whole,
            terms_holding(&terms, &listed[range.0].id),
        ));
        agent_service::inject_mock_answer(ROUTE, &marker, 1, &part.to_string());
    }

    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(
        requests() - before,
        HANDLER_MODULES + 1 + 3 + 2,
        "the whole request, three parts, and the two halves of the part that was cut"
    );
    assert_eq!(
        bytes(&result.raw_model_results[&fx.table]),
        stored_form(&whole),
        "the halves' rows take their place among the other parts'"
    );
    assert_eq!(lost_reason(&fx.table), None);
}

#[tokio::test]
#[serial]
async fn a_single_group_that_is_cut_loses_the_file_by_name() {
    offline();
    let tag = "single";
    let fx = Fixture::table(tag);

    // The last registration's request is cut however few candidates ride
    // with it: in the last part (8 groups), in its back half (4), in that
    // half's back half (2), and alone.
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    agent_service::inject_mock_envelope(ROUTE, &single_marker(tag, SINGLES - 1), 4, &cut());

    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(
        requests() - before,
        HANDLER_MODULES + 1 + 3 + 2 + 2 + 2,
        "the whole request, three parts, then two halves at each of three halvings"
    );
    assert!(
        !result.raw_model_results.contains_key(&fx.table),
        "no partial answer is stored"
    );
    assert_eq!(
        lost_reason(&fx.table).as_deref(),
        Some("output_truncated"),
        "the file is lost by name, with the verdict as the reason"
    );
    assert_eq!(result.stats.files_analysis_failed, 1);
    assert!(
        result.file_results.contains_key(&fx.table),
        "the rows the source states are kept, as for any file the model did not answer"
    );
}

#[tokio::test]
#[serial]
async fn a_part_lost_for_another_reason_loses_the_file_with_that_reason() {
    offline();
    let tag = "partlost";
    let fx = Fixture::table(tag);

    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    agent_service::inject_mock_failure(ROUTE, &single_marker(tag, LOCAL_SINGLES), 1);

    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(
        requests() - before,
        HANDLER_MODULES + 1 + 3,
        "the whole request and three parts; a part lost to the model is not asked again"
    );
    assert!(
        !result.raw_model_results.contains_key(&fx.table),
        "no partial answer is stored"
    );
    assert_eq!(lost_reason(&fx.table).as_deref(), Some("model_error"));
}

/// carrick#1909: a part the model refuses for capacity is the provider being
/// busy, and no verdict on the file. The file is owed under the refusal's
/// code and counted as refused for capacity, so the run's retry waits and
/// asks again. It is not counted as a final answer, although the verdict
/// that split the file was one, and nothing of the parts that did answer is
/// stored.
#[tokio::test]
#[serial]
async fn a_part_refused_for_capacity_leaves_the_file_owed_and_not_final() {
    offline();
    let tag = "partbusy";
    let fx = Fixture::table(tag);

    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    agent_service::inject_mock_envelope(
        ROUTE,
        &single_marker(tag, LOCAL_SINGLES),
        1,
        &refusal("model_error", true),
    );

    let result = fx.analyze().await;
    let losses = carrick::scan_health::service_losses(None);
    assert_eq!(lost_reason(&fx.table).as_deref(), Some("model_error"));
    assert_eq!(
        (losses.files, losses.capacity_files, losses.final_files),
        (1, 1, 0),
        "{losses:?}"
    );
    assert!(
        !result.raw_model_results.contains_key(&fx.table),
        "no partial answer is stored"
    );
}

/// carrick#1909: a part that is not sent, because the route had stopped
/// answering and had just refused another request, leaves the file owed the
/// same way, under the scanner's own code for a call that was never sent.
/// The part is not one of the run's requests: the whole request and the two
/// parts that were sent are.
#[tokio::test]
#[serial]
async fn a_part_that_is_never_sent_leaves_the_file_owed_and_is_not_a_request() {
    offline();
    let tag = "partunsent";
    let fx = Fixture::table(tag);

    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    agent_service::inject_mock_not_asked(ROUTE, &single_marker(tag, LOCAL_SINGLES), 1);

    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(
        requests() - before,
        HANDLER_MODULES + 1 + 2,
        "the whole request and the two parts that were sent"
    );
    let losses = carrick::scan_health::service_losses(None);
    assert_eq!(
        lost_reason(&fx.table).as_deref(),
        Some(agent_service::NOT_ASKED_CODE)
    );
    assert_eq!(
        (losses.files, losses.capacity_files, losses.final_files),
        (1, 1, 0),
        "{losses:?}"
    );
    assert!(
        !result.raw_model_results.contains_key(&fx.table),
        "no partial answer is stored"
    );
}

/// The lines of a prompt's FILE CONTENT section: its header, the lines
/// between the header and the fence, and the lines the fence holds.
struct FileSection<'a> {
    header: &'a str,
    under_header: Vec<&'a str>,
    fence: Vec<&'a str>,
}

fn file_section(prompt: &str) -> FileSection<'_> {
    let start = prompt.find("### FILE CONTENT").expect("the header");
    let end = prompt[start..]
        .find("\n```\n\nAnalyze this file")
        .expect("the fence's close");
    let mut lines = prompt[start..start + end].lines();
    let header = lines.next().unwrap();
    let mut under_header = Vec::new();
    for line in lines.by_ref() {
        if line == "```" {
            break;
        }
        under_header.push(line);
    }
    FileSection {
        header,
        under_header,
        fence: lines.collect(),
    }
}

/// The number a line of the fence carries, as the cloud's pre-call gate reads
/// one (`^\s*\d+[:|] ?`, carrick-cloud#2266), or `None` for a line with none.
fn line_number(line: &str) -> Option<u32> {
    let digits = line.trim_start();
    let end = digits.find(|c: char| !c.is_ascii_digit())?;
    (end > 0 && digits[end..].starts_with(['|', ':']))
        .then(|| digits[..end].parse().ok())
        .flatten()
}

/// What every part prompt holds whatever the file: the three conditions the
/// cloud's reading of a part rests on (carrick-cloud#2266), and that the
/// fence shows exactly the lines the part's cut says it shows.
fn assert_a_part_prompt_shows(prompt: &str, whole: &str, part: &SourcePart, source: &str) {
    let section = file_section(prompt);
    let whole_section = file_section(whole);
    // 1. The header line is the whole prompt's, byte for byte.
    assert_eq!(section.header, whole_section.header);
    // 2. The sentence that states the window sits outside the header, on the
    //    one line the part adds under it, and says what is not shown.
    let [same, sentence] = section.under_header.as_slice() else {
        panic!("two lines under the header: {:?}", section.under_header);
    };
    assert_eq!([*same], whole_section.under_header.as_slice());
    let (first, last) = part.window();
    let shown = part.shown();
    let (a, b) = (
        shown.prompt_line(first).unwrap(),
        shown.prompt_line(last).unwrap(),
    );
    assert!(
        sentence.starts_with(&format!(
            "This request covers lines {a} to {b} below, one part of the file."
        )),
        "{sentence}"
    );
    assert!(
        sentence.contains("the numbers count the lines shown here")
            || sentence.contains("The numbers count the lines shown here"),
        "{sentence}"
    );
    assert!(
        sentence.contains("the rest of the file is not shown")
            && sentence.contains(&format!(
                "no entry is owed for anything outside lines {a} to {b}"
            )),
        "{sentence}"
    );
    // 3. Lines with no number are under one in ten of the lines shown.
    let numbered: Vec<u32> = section
        .fence
        .iter()
        .filter_map(|line| line_number(line))
        .collect();
    let unnumbered = section.fence.len() - numbered.len();
    assert!(
        unnumbered * 10 < numbered.len() || unnumbered == 0,
        "{unnumbered} line(s) with no number among {}",
        numbered.len()
    );
    // The fence shows the window, what the cut says is shown beside it, and
    // no other line of the file, each as the whole prompt writes it.
    let expected: Vec<u32> = shown
        .runs
        .iter()
        .flat_map(|(first, last)| *first..=*last)
        .map(|line| shown.prompt_line(line).unwrap())
        .collect();
    assert_eq!(numbered, expected, "part {}", part.label());
    // The part's numbers count from 1 and skip one number a gap.
    assert_eq!(numbered[0], 1);
    assert_eq!(
        numbered.len() as u32 + shown.runs.len() as u32 - 1,
        *numbered.last().unwrap(),
        "one number skipped at each gap"
    );
    assert!((first..=last).all(|line| shown.holds(line)));
    let lines: Vec<&str> = source.lines().collect();
    for line in &section.fence {
        if let Some(number) = line_number(line) {
            let file = shown.file_line(number.into()).unwrap();
            assert_eq!(
                *line,
                format!("{number:4}| {}", lines[file as usize - 1]),
                "prompt line {number}"
            );
        }
    }
}

/// The names a prompt's import table lists.
fn imported_names(prompt: &str) -> Vec<String> {
    section(prompt, "IMPORT TABLE")
        .lines()
        .filter_map(|line| line.split_once("': "))
        .flat_map(|(_, names)| {
            names
                .rsplit_once(" [")
                .map_or(names, |(names, _)| names)
                .split(", ")
                .map(|name| name.split(" as ").last().unwrap().trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
#[serial]
async fn a_part_prompt_shows_its_window_and_lists_its_own_candidates_imports_and_modules() {
    offline();
    let tag = "prompt";
    let fx = Fixture::table(tag);
    let whole = fx.whole_prompt().await;
    let listed = listed_in(&whole);
    assert_the_table_lists_what_the_tests_assume(&listed);
    let source = table_source(tag);
    let windows = source_parts(
        Path::new(&fx.table_prompt_path),
        &source,
        &as_listed_candidates(&listed),
        &LIMITS,
    )
    .expect("the table parses");
    assert_eq!(
        windows.iter().map(SourcePart::window).collect::<Vec<_>>(),
        PART_WINDOWS
    );
    let every_module: Vec<String> = (0..HANDLER_MODULES)
        .map(|n| format!("src/handlers/h{n}.ts"))
        .collect();
    assert_eq!(
        attached_modules(&whole),
        every_module,
        "the whole prompt carries every module the table imports"
    );

    let dump = tempfile::tempdir().unwrap();
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    dump_into(Some(dump.path()));
    let before = requests();
    fx.analyze().await;
    dump_into(None);
    assert_eq!(requests() - before, HANDLER_MODULES + 1 + 3);

    let dumps = fx.dumps(dump.path());
    assert!(
        dumps.whole.is_none(),
        "a request that was cut has no answer to dump"
    );
    let parts: Vec<&String> = dumps.parts.values().collect();
    assert_eq!(parts.len(), 3, "part dumps: {:?}", dumps.parts.keys());

    // What each part's candidates name through an import: the first part's
    // handlers are the table's own, the middle part's come from all four
    // modules, the last part's from the first.
    let modules: [&[String]; 3] = [&[], &every_module, &every_module[..1]];
    // What each part's shown lines name of the table's four imports: the
    // first part's window holds the import lines themselves, the middle
    // part's registrations name all four, the last part's name one.
    let imports: [&[&str]; 3] = [
        &["h0", "h1", "h2", "h3"],
        &["h0", "h1", "h2", "h3"],
        &["h0"],
    ];
    // The declarations each part shows beside its window: the first part's
    // window holds every one it uses; the middle part uses the table, which
    // is made by a function declared above it; the last part the same.
    let context: [&[(u32, u32)]; 3] = [&[], &[(5, 5), (6, 6)], &[(5, 5), (6, 6)]];
    for (index, ((part, (from, to)), modules)) in
        parts.iter().zip(PART_RANGES).zip(modules).enumerate()
    {
        assert_a_part_prompt_shows(part, &whole, &windows[index], &source);
        assert_eq!(windows[index].context, context[index], "part {index}");
        let mut names = imported_names(part);
        names.sort();
        assert_eq!(names, imports[index], "part {index}'s import table");
        assert!(
            part.len() < whole.len(),
            "a part is smaller than the request that was cut"
        );
        let ids: Vec<String> = (1..=to - from).map(|n| format!("c{n}")).collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let in_context: Vec<String> = listed_in(part).into_iter().map(|c| c.id).collect();
        assert_eq!(in_context, ids, "the part's candidate contexts");
        assert!(
            !part.contains("span_start") && !part.contains("(span "),
            "a part states no position in the file"
        );
        let in_targets: Vec<&str> = section(part, "CANDIDATE TARGETS")
            .lines()
            .filter_map(|line| line.strip_prefix("- Candidate "))
            .map(|rest| rest.split(": Line ").next().unwrap())
            .collect();
        assert_eq!(in_targets, ids, "the part's candidate targets");

        assert_eq!(attached_modules(part), modules, "the part's modules");
        assert_eq!(
            part.contains("### IMPORTED HTTP WRAPPER DEFINITIONS"),
            !modules.is_empty(),
            "a part that names no module carries no such section"
        );
        for module in modules {
            let header = format!("--- wrapper module: {module} ---\n");
            let source = handler_source(every_module.iter().position(|m| m == module).unwrap());
            assert!(
                part.contains(&format!("{header}{source}")),
                "{module} is attached as the whole prompt attaches it"
            );
        }

        assert_eq!(
            outside_the_part_sections(part),
            outside_the_part_sections(&whole),
            "guidance and instructions are the whole prompt's"
        );
    }
}

/// The cloud may state the part size on the verdict. The file is then asked
/// in parts of at most that many candidates.
#[tokio::test]
#[serial]
async fn a_verdict_that_states_a_part_size_is_asked_in_parts_of_that_size() {
    offline();
    let fx = Fixture::table("stated");
    let (sent, sizes) = requests_and_part_sizes(&fx, &cut_stating(json!(16))).await;

    // 30 singles, the chain of three, 37 singles, at 16 a part: the chain
    // cannot join the 14 singles in front of it, and it is not cut to fit.
    assert_eq!(sizes, [16, 14, 16, 16, 8]);
    assert_eq!(
        sizes.iter().max(),
        Some(&16),
        "no part holds more than the verdict states"
    );
    assert_eq!(
        sent,
        HANDLER_MODULES + 1 + 5,
        "the whole request and five parts"
    );
}

/// A verdict that states no part size, or states one that is no count, is
/// asked in parts of the scanner's own limit.
#[tokio::test]
#[serial]
async fn a_verdict_that_states_no_part_size_is_asked_in_parts_of_thirty_two() {
    for (tag, verdict) in [
        ("unstated", cut()),
        ("zero", cut_stating(json!(0))),
        ("words", cut_stating(json!("sixteen"))),
    ] {
        offline();
        let fx = Fixture::table(tag);
        let (sent, sizes) = requests_and_part_sizes(&fx, &verdict).await;
        assert_eq!(sizes, [30, 32, 8], "{tag}");
        assert_eq!(sizes.iter().max(), Some(&MAX_PART_CANDIDATES), "{tag}");
        assert_eq!(
            sent,
            HANDLER_MODULES + 1 + 3,
            "{tag}: the whole request and three parts"
        );
    }
}

#[tokio::test]
#[serial]
async fn a_file_that_answers_whole_sends_one_request() {
    offline();
    let fx = Fixture::table("answers");
    let dump = tempfile::tempdir().unwrap();
    dump_into(Some(dump.path()));
    let before = requests();
    let result = fx.analyze().await;
    dump_into(None);

    assert_eq!(
        requests() - before,
        HANDLER_MODULES + 1,
        "one request a file, however many candidates it holds"
    );
    assert_eq!(result.stats.files_model_dispatched, HANDLER_MODULES + 1);
    let dumps = fx.dumps(dump.path());
    assert!(dumps.whole.is_some() && dumps.parts.is_empty());
    assert!(
        listed_in(&dumps.whole.unwrap()).len() > MAX_PART_CANDIDATES,
        "the file holds more candidates than a part may"
    );
}

/// A whole request that fails for any reason but the final cut verdict is not
/// split: the file is lost with that reason, after one request.
#[tokio::test]
#[serial]
async fn a_file_lost_for_any_other_reason_is_not_split() {
    for (code, retriable) in [
        ("model_error", true),
        ("gateway_error", true),
        ("bad_response", false),
        // The cut is the verdict only when the cloud says it is final.
        ("output_truncated", true),
    ] {
        offline();
        let tag = format!("lost{}{}", code.replace('_', ""), retriable);
        let fx = Fixture::table(&tag);
        agent_service::inject_mock_envelope(
            ROUTE,
            &fx.whole_marker(),
            1,
            &refusal(code, retriable),
        );

        let before = requests();
        let result = fx.analyze().await;
        assert_eq!(
            requests() - before,
            HANDLER_MODULES + 1,
            "{code} (retriable={retriable}): the file is not asked again"
        );
        assert!(!result.raw_model_results.contains_key(&fx.table), "{code}");
        assert_eq!(lost_reason(&fx.table).as_deref(), Some(code));
    }
}

#[tokio::test]
#[serial]
async fn a_file_with_no_candidates_is_cut_by_size_alone_and_halved_by_items() {
    offline();
    // A file that raises no candidate and is asked about because it imports a
    // module that makes a call: an import and four declarations that use it.
    let mut source = String::from("import { h0 } from \"./handlers/h0\";\n");
    for i in 0..4 {
        writeln!(
            source,
            "export const nocandidatesUse{i} = () => h0({i} as any, {i} as any);"
        )
        .unwrap();
    }
    let fx = Fixture::of(
        &[
            ("src/handlers/h0.ts".to_string(), handler_source(0)),
            ("src/nocandidates_uses.ts".to_string(), source.clone()),
        ],
        "src/nocandidates_uses.ts",
    );
    let whole = fx.whole_prompt().await;
    assert!(listed_in(&whole).is_empty(), "the file lists no candidate");
    let [one] = source_parts(Path::new(&fx.table_prompt_path), &source, &[], &LIMITS)
        .expect("the file parses")
        .try_into()
        .expect("a file this small is one window");
    assert_eq!(one.window(), (1, 5));

    // The whole request is cut. Its one window would repeat that request, so
    // it is halved between its items without being sent: two lines and three.
    // The back half is cut too, and is halved again: one line and two.
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    agent_service::inject_mock_envelope(ROUTE, "nocandidatesUse3 = ", 1, &cut());
    let dump = tempfile::tempdir().unwrap();
    dump_into(Some(dump.path()));
    let before = requests();
    let result = fx.analyze().await;
    dump_into(None);
    assert_eq!(
        requests() - before,
        1 + 1 + 2 + 2,
        "the module, the whole request, its two halves, and the halves of the one that was cut"
    );
    assert!(
        result.raw_model_results.contains_key(&fx.table),
        "the file has its answer"
    );
    assert_eq!(lost_reason(&fx.table), None);

    let dumps = fx.dumps(dump.path());
    assert_eq!(
        dumps.parts.keys().collect::<Vec<_>>(),
        ["000001-000002", "000003-000003", "000004-000005"],
        "the parts that answered, named by their windows"
    );
    for (label, part) in &dumps.parts {
        assert!(listed_in(part).is_empty(), "{label}");
        // Every part of a file with no candidate carries the module the file
        // imports: that module is why the file is asked about.
        assert_eq!(attached_modules(part), ["src/handlers/h0.ts"], "{label}");
        assert_eq!(imported_names(part), ["h0"], "{label}");
    }
}

/// A part that would repeat the request that was just cut is not sent: its
/// verdict is already known.
#[tokio::test]
#[serial]
async fn a_request_that_was_cut_is_never_sent_again() {
    offline();
    // One registration: its only part is the whole request.
    let one = Fixture::of(
        &[(
            "src/onegroup_table.ts".to_string(),
            "declare function makeTable(): any;\nconst table = makeTable();\ntable.get(\"/onegroup/r/0\", () => {});\n"
                .to_string(),
        )],
        "src/onegroup_table.ts",
    );
    assert_eq!(one.listed().await.len(), 1);
    agent_service::inject_mock_envelope(ROUTE, &one.whole_marker(), 1, &cut());
    let before = requests();
    let result = one.analyze().await;
    assert_eq!(requests() - before, 1, "the cut request is the only one");
    assert!(!result.raw_model_results.contains_key(&one.table));
    assert_eq!(lost_reason(&one.table).as_deref(), Some("output_truncated"));

    // Six registrations and no imported module: one part, whose prompt is the
    // whole prompt. It is halved without being sent.
    let mut source =
        String::from("declare function makeTable(): any;\nconst table = makeTable();\n");
    for i in 0..6 {
        writeln!(source, "table.get(\"/sixgroups/r/{i}\", () => {{}});").unwrap();
    }
    let six = Fixture::of(
        &[("src/sixgroups_table.ts".to_string(), source)],
        "src/sixgroups_table.ts",
    );
    assert_eq!(six.listed().await.len(), 6);
    agent_service::inject_mock_envelope(ROUTE, &six.whole_marker(), 1, &cut());
    let before = requests();
    let result = six.analyze().await;
    assert_eq!(
        requests() - before,
        1 + 2,
        "the cut request, then its two halves"
    );
    assert!(result.raw_model_results.contains_key(&six.table));
}

/// Nothing a mocked scan answers is the verdict, so a mocked scan sends what
/// it sent before: one request a file with something to ask about.
#[tokio::test]
#[serial]
async fn a_mocked_scan_sends_one_request_a_file() {
    offline();
    let fx = Fixture::table("mocked");
    std::fs::write(
        fx.root.join("package.json"),
        r#"{"name":"mocked-table","version":"1.0.0"}"#,
    )
    .unwrap();

    let before = agent_service::request_counts();
    carrick::engine::run_analysis_engine_with_sidecar(
        carrick::cloud_storage::MockStorage::new(),
        fx.root.to_str().unwrap(),
        None,
        true,
    )
    .await
    .expect("a mocked scan");
    let after = agent_service::request_counts();

    let sent = |route: &str| {
        after.get(route).copied().unwrap_or(0) - before.get(route).copied().unwrap_or(0)
    };
    assert_eq!(
        sent(ROUTE),
        HANDLER_MODULES + 1,
        "one request a file: {}",
        agent_service::requests_between(&before, &after)
    );
}

/// The text only candidate `i` of the generated `flat_module` carries in a
/// request, which a part's candidate ids (`c1`, `c2`, ...) cannot be.
fn flat_module_marker(tag: &str, i: usize) -> String {
    format!("[path: '/{tag}/items/{i}']")
}

/// Answer each of `parts` with a call row for each of its candidates, rows
/// last first: the order a part's rows arrive in is the model's. A part is
/// answered in what its prompt shows: its candidate ids and its line numbers
/// (carrick#2794). `marker_of` names the text only a part's first candidate
/// carries, by its place in the file.
fn answer_parts_with_call_rows(
    dumps: &Dumps,
    parts: &[SourcePart],
    listed: &[Listed],
    marker_of: impl Fn(usize) -> String,
) {
    for part in parts {
        let terms = &dumps.terms[&part.label()];
        let whole = json!({
            "mounts": [],
            "endpoints": [],
            "data_calls": part
                .candidates
                .iter()
                .rev()
                .map(|position| data_call_row(&listed[*position], *position))
                .collect::<Vec<_>>(),
        });
        let answer = in_part_terms(&whole, terms);
        agent_service::inject_mock_answer(
            ROUTE,
            &marker_of(part.candidates[0]),
            1,
            &answer.to_string(),
        );
    }
}

/// carrick#2308: a part's request holds its window and the declarations the
/// window uses, and nothing else of the file.
///
/// A generated module of 400 one-request functions under one import line of
/// several hundred names. Its whole prompt is over the line, so it is asked
/// in parts from its first request (carrick#2309), and every part answers
/// with a call row for each of its candidates.
#[tokio::test]
#[serial]
async fn a_part_sends_its_window_and_not_the_file() {
    offline();
    let tag = "windowed";
    const FUNCTIONS: usize = 400;
    let shape = shapes::flat_module(tag, FUNCTIONS);
    let subject = format!("src/{tag}_client.ts");
    // The whole prompt of a small file at the same path: its header and the
    // line under it are what every part of the large one must repeat.
    let whole = Fixture::of(
        &[(subject.clone(), shapes::flat_module(tag, 3).source)],
        &subject,
    )
    .whole_prompt()
    .await;
    let fx = Fixture::of(&[(subject.clone(), shape.source.clone())], &subject);
    let (first, _) = fx.dumped_run().await;
    assert!(first.whole.is_none(), "the file is never sent whole");
    let listed = listed_in_parts(&first);
    assert_eq!(listed.len(), FUNCTIONS, "one candidate a function");
    assert!(
        listed
            .windows(2)
            .all(|pair| pair[0].span_start < pair[1].span_start),
        "listed in source order"
    );
    let parts = source_parts(
        Path::new(&subject),
        &shape.source,
        &as_listed_candidates(&listed),
        &LIMITS,
    )
    .expect("the module parses");
    assert_eq!(
        parts.len(),
        FUNCTIONS.div_ceil(MAX_PART_CANDIDATES),
        "32 candidates a part"
    );

    answer_parts_with_call_rows(&first, &parts, &listed, |i| flat_module_marker(tag, i));
    let dump = tempfile::tempdir().unwrap();
    dump_into(Some(dump.path()));
    let before = requests();
    let result = fx.analyze().await;
    dump_into(None);
    assert_eq!(requests() - before, parts.len(), "one request a part");

    let dumps = fx.dumps(dump.path());
    assert_eq!(
        dumps.parts.keys().cloned().collect::<Vec<_>>(),
        parts.iter().map(SourcePart::label).collect::<Vec<_>>()
    );
    // The line each function's request is on.
    let mut request_lines = Vec::with_capacity(FUNCTIONS);
    let (mut line, mut read_to) = (1u32, 0);
    for start in &shape.starts {
        line += shape.source[read_to..*start].matches('\n').count() as u32;
        read_to = *start;
        request_lines.push(line);
    }
    let mut largest = 0;
    for (part, prompt) in parts.iter().zip(dumps.parts.values()) {
        assert_a_part_prompt_shows(prompt, &whole, part, &shape.source);
        // A request's path is in the prompt exactly when its line is shown:
        // no hint, no context and no line of source names any other.
        let shown = part.shown();
        for (i, line) in request_lines.iter().enumerate() {
            assert_eq!(
                prompt.contains(&format!("/{tag}/items/{i}\"")),
                shown.holds(*line),
                "part {}: request {i} on line {line}",
                part.label()
            );
        }
        // The import table lists a name only when a shown line names it.
        let mut names = imported_names(prompt);
        names.sort();
        let mut expected: Vec<String> = (0..shapes::IMPORTED_NAMES)
            .map(shapes::imported_name)
            .filter(|name| {
                shown.holds(1)
                    || request_lines.iter().enumerate().any(|(i, line)| {
                        // A function's return type is on the line above its
                        // request.
                        shown.holds(line - 1) && shapes::imported_name(i) == *name
                    })
            })
            .collect();
        expected.sort();
        assert_eq!(names, expected, "part {}", part.label());
        largest = largest.max(prompt.len());
    }
    assert!(
        largest < 130_000,
        "the largest part request is {largest} bytes"
    );

    let stored = &result.raw_model_results[&fx.table];
    let targets: Vec<&str> = stored
        .data_calls
        .iter()
        .map(|row| row.target.as_str())
        .collect();
    let expected: Vec<String> = (0..FUNCTIONS).map(|i| format!("/called/{i}")).collect();
    assert_eq!(targets, expected, "400 call rows in source order");
    assert_eq!(lost_reason(&fx.table), None);
}

/// carrick#2309: a file whose whole prompt is over the line is asked in
/// parts from its first request, no request is over the line, the parts'
/// answers are stored as one, and a scan over that stored answer sends
/// nothing.
#[tokio::test]
#[serial]
async fn a_file_over_the_line_is_never_sent_whole() {
    offline();
    let tag = "overline";
    const FUNCTIONS: usize = 400;
    let shape = shapes::flat_module(tag, FUNCTIONS);
    let subject = format!("src/{tag}_client.ts");
    let fx = Fixture::of(&[(subject.clone(), shape.source.clone())], &subject);

    let (first, sent) = fx.dumped_run().await;
    assert!(first.whole.is_none(), "no whole request");
    assert_eq!(sent, first.parts.len(), "the requests are the parts");
    for (label, prompt) in &first.parts {
        assert!(
            prompt.len() <= WHOLE_PROMPT_MAX_BYTES,
            "part {label} is {} bytes",
            prompt.len()
        );
    }
    let listed = listed_in_parts(&first);
    assert_eq!(listed.len(), FUNCTIONS);
    let parts = source_parts(
        Path::new(&subject),
        &shape.source,
        &as_listed_candidates(&listed),
        &LIMITS,
    )
    .expect("the module parses");
    assert_eq!(
        first.parts.keys().cloned().collect::<Vec<_>>(),
        parts.iter().map(SourcePart::label).collect::<Vec<_>>(),
        "the parts asked are the file's windows"
    );

    answer_parts_with_call_rows(&first, &parts, &listed, |i| flat_module_marker(tag, i));
    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(requests() - before, parts.len());
    assert_eq!(
        result.stats.files_model_dispatched, 1,
        "a file is counted once"
    );
    let stored = &result.raw_model_results[&fx.table];
    assert_eq!(
        stored.data_calls.len(),
        FUNCTIONS,
        "one stored answer, 400 rows"
    );
    assert_eq!(lost_reason(&fx.table), None);

    let before = requests();
    let again = fx
        .analyze_with(&result.raw_model_results, &Default::default())
        .await;
    assert_eq!(requests() - before, 0, "the stored answer is replayed");
    assert_eq!(again.stats.files_model_reused, 1);
}

/// carrick#2309: a file whose whole prompt is at the line is sent whole, in
/// the bytes it was always sent in, and one byte more asks it in parts.
#[tokio::test]
#[serial]
async fn a_file_at_the_line_is_sent_whole() {
    offline();
    let tag = "atline";
    let subject = format!("src/{tag}_client.ts");
    // A request, then a declaration under a comment that pads the file.
    let source = |padding: usize| {
        format!(
            "declare const client: any;\n\
             export function readOne(id: string) {{\n  \
             return client.get({{ url: \"/{tag}/items/0\", params: {{ id }} }});\n}}\n\
             // {}\n\
             export const padding = 0;\n",
            "p".repeat(padding)
        )
    };
    let whole_of = |padding: usize| {
        let subject = subject.clone();
        let source = source(padding);
        async move {
            let fx = Fixture::of(&[(subject.clone(), source)], &subject);
            fx.dumped_run().await
        }
    };
    let (unpadded, _) = whole_of(0).await;
    let base = unpadded.whole.expect("a small file is sent whole").len();
    let padding = WHOLE_PROMPT_MAX_BYTES - base;

    let (at, sent) = whole_of(padding).await;
    assert_eq!(sent, 1, "one request");
    assert!(at.parts.is_empty());
    let at_whole = at.whole.expect("a file at the line is sent whole");
    assert_eq!(at_whole.len(), WHOLE_PROMPT_MAX_BYTES);
    assert!(
        at_whole.contains(&format!("// {}\n", "p".repeat(padding))),
        "the whole file, in the whole prompt's bytes"
    );

    let (over, _) = whole_of(padding + 1).await;
    assert!(over.whole.is_none(), "one byte more is not sent whole");
    assert!(!over.parts.is_empty());
    assert!(
        over.parts
            .values()
            .all(|prompt| prompt.len() <= WHOLE_PROMPT_MAX_BYTES)
    );
}

/// carrick#2309: a module whose one exported array literal holds 400 route
/// objects and is most of the file is cut between the literal's elements,
/// and the stored answer holds every route once.
#[tokio::test]
#[serial]
async fn the_route_table_literal_keeps_every_route() {
    offline();
    let tag = "routetable";
    const ROUTES: usize = 400;
    let shape = shapes::route_array(tag, ROUTES);
    let subject = format!("src/{tag}.ts");
    let fx = Fixture::of(&[(subject.clone(), shape.source.clone())], &subject);
    let (first, _) = fx.dumped_run().await;
    assert!(first.whole.is_none(), "the file is over the line");
    let listed = listed_in_parts(&first);
    assert_eq!(listed.len(), ROUTES, "one candidate a route");
    let parts = source_parts(
        Path::new(&subject),
        &shape.source,
        &as_listed_candidates(&listed),
        &LIMITS,
    )
    .expect("the module parses");
    assert!(parts.len() > 1);

    // Each part answers the routes the generator states for its candidates,
    // in the ids and line numbers its prompt shows.
    for part in &parts {
        let rows: Vec<Value> = part
            .candidates
            .iter()
            .map(|position| {
                let mut row = endpoint_row(&listed[*position], *position);
                row["path"] = json!(format!("/{tag}/routes/{position}"));
                row["handler_name"] = json!(shapes::route_array_handler(*position));
                row
            })
            .collect();
        let whole = json!({ "mounts": [], "endpoints": rows, "data_calls": [] });
        let answer = in_part_terms(&whole, &first.terms[&part.label()]);
        // The text of the part's first route, which no other part carries.
        let marker = format!(
            "res.json({{ service: SERVICE, route: {} }})",
            part.candidates[0]
        );
        agent_service::inject_mock_answer(ROUTE, &marker, 1, &answer.to_string());
    }
    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(requests() - before, parts.len());
    let stored = &result.raw_model_results[&fx.table];
    let routes: Vec<(&str, &str)> = stored
        .endpoints
        .iter()
        .map(|row| (row.path.as_str(), row.handler_name.as_str()))
        .collect();
    let expected: Vec<(String, String)> = (0..ROUTES)
        .map(|i| (format!("/{tag}/routes/{i}"), shapes::route_array_handler(i)))
        .collect();
    assert_eq!(
        routes,
        expected
            .iter()
            .map(|(path, handler)| (path.as_str(), handler.as_str()))
            .collect::<Vec<_>>(),
        "400 route rows, once each, in source order"
    );
}

/// A module of `requests` one-request functions after `head`, under one
/// ambient declaration of the client they call.
fn requests_after(tag: &str, head: &str, requests: usize) -> String {
    let mut source = String::from("declare const client: any;\n");
    source.push_str(head);
    for i in 0..requests {
        writeln!(
            source,
            "export function read{i}(id: string) {{\n  return client.get({{ url: \"/{tag}/items/{i}\", params: {{ id }} }});\n}}"
        )
        .unwrap();
    }
    source
}

/// carrick#2309: an item with no list to cut that is over the line on its
/// own, and holds no candidate, is left out; the rest of the file is asked.
#[tokio::test]
#[serial]
async fn an_item_that_cannot_be_cut_and_holds_no_candidate_is_left_out() {
    offline();
    let tag = "leftout";
    let blob = "leftoutblob".repeat(300_000 / 11);
    let source = requests_after(tag, &format!("export const BLOB = \"{blob}\";\n"), 4);
    let subject = format!("src/{tag}.ts");
    let fx = Fixture::of(&[(subject.clone(), source)], &subject);

    let (dumps, sent) = fx.dumped_run().await;
    assert!(dumps.whole.is_none(), "the file is over the line");
    assert_eq!(sent, 1, "only the window of requests is asked");
    let [prompt] = dumps.parts.values().collect::<Vec<_>>()[..] else {
        panic!("one part: {:?}", dumps.parts.keys());
    };
    assert!(!prompt.contains("leftoutblob"), "the literal is not sent");
    assert_eq!(listed_in(prompt).len(), 4, "every request is asked");

    let listed = listed_in(prompt);
    let rows: Vec<Value> = listed
        .iter()
        .enumerate()
        .map(|(position, candidate)| data_call_row(candidate, position))
        .collect();
    agent_service::inject_mock_answer(
        ROUTE,
        &format!("- Candidate {}: ", listed[0].id),
        1,
        &json!({ "mounts": [], "endpoints": [], "data_calls": rows }).to_string(),
    );
    let result = fx.analyze().await;
    assert_eq!(result.raw_model_results[&fx.table].data_calls.len(), 4);
    assert_eq!(lost_reason(&fx.table), None);
}

/// carrick#2309: a candidate inside an item that cannot be cut under the line
/// names the file under its own reason. No request is sent, the file keeps
/// its deterministic rows, nothing is stored as its answer, and the next scan
/// decides the same.
#[tokio::test]
#[serial]
async fn a_candidate_inside_an_item_that_cannot_be_cut_names_the_file() {
    offline();
    let tag = "uncuttable";
    let body = "uncuttablebody".repeat(300_000 / 14);
    let source = format!(
        "declare const client: any;\nexport function sendBig() {{\n  return client.post({{ url: \"/{tag}/items/0\", body: \"{body}\" }});\n}}\n"
    );
    let subject = format!("src/{tag}.ts");
    let fx = Fixture::of(&[(subject.clone(), source)], &subject);

    for scan in ["first", "next"] {
        offline();
        let before = requests();
        let result = fx.analyze().await;
        assert_eq!(requests() - before, 0, "{scan} scan: no request");
        assert_eq!(
            lost_reason(&fx.table).as_deref(),
            Some(FILE_TOO_LARGE_CODE),
            "{scan} scan"
        );
        assert!(
            !result.raw_model_results.contains_key(&fx.table),
            "{scan} scan: nothing is stored as its answer"
        );
        let calls: Vec<&str> = result.file_results[&fx.table]
            .data_calls
            .iter()
            .map(|row| row.target.as_str())
            .collect();
        assert!(
            calls
                .iter()
                .any(|target| target.contains(&format!("/{tag}/items/0"))),
            "{scan} scan: the deterministic call row is kept: {calls:?}"
        );
    }
    assert!(
        carrick::scan_health::final_losses_for(None)
            .iter()
            .any(|lost| lost.path == fx.table),
        "the file is named, as lost for good"
    );
    assert!(
        !carrick::scan_health::unanalysed_files_for(None)
            .iter()
            .any(|lost| lost.path == fx.table),
        "and is not owed (carrick#2488)"
    );
}

/// How many plugin modules the large wiring module registers: enough that
/// their attached sources, each at the cap, are over the line together.
const PLUGINS: usize = 70;

/// How many the small one registers: more than a part's candidates, so the
/// registration's callback runs past the part that lists it.
const SMALL_PLUGINS: usize = 40;

/// A plugin module: one route, and when `padded` a comment that runs the
/// module past the cap its source is attached under.
fn plugin_source(tag: &str, n: usize, padded: bool) -> String {
    let padding = if padded {
        "pad ".repeat(WRAPPER_SNIPPET_MAX / 4)
    } else {
        String::new()
    };
    format!(
        "export async function plugin{n}(app: any) {{\n  app.get(\"/{tag}/p/{n}\", async () => ({{ plugin: {n} }}));\n}}\n// {padding}\n"
    )
}

/// A wiring module: an import per plugin, a data table longer than a window,
/// and one registration whose inline callback registers every plugin.
fn wiring_source(tag: &str, plugins: usize) -> String {
    let mut out = String::new();
    for n in 0..plugins {
        writeln!(out, "import {{ plugin{n} }} from \"./plugins/plugin{n}\";").unwrap();
    }
    out.push_str("export const notes = [\n");
    for i in 0..(PART_WINDOW_BYTES / 60 + 100) {
        writeln!(
            out,
            "  \"note {i:05}: a line of data that runs nothing at all\","
        )
        .unwrap();
    }
    out.push_str("];\n");
    out.push_str("export async function wire(server: any) {\n");
    out.push_str("  await server.register(async (api: any) => {\n");
    out.push_str("    const settings = { ready: true };\n");
    for n in 0..plugins {
        writeln!(out, "    await api.register(plugin{n});").unwrap();
    }
    writeln!(out, "  }}, {{ prefix: \"/{tag}\" }});").unwrap();
    out.push_str("}\n");
    out
}

/// A wiring module registering `plugins` plugin modules, and its source.
fn wiring_fixture(tag: &str, plugins: usize, padded: bool) -> (Fixture, String) {
    let mut modules: Vec<(String, String)> = (0..plugins)
        .map(|n| {
            (
                format!("src/plugins/plugin{n}.ts"),
                plugin_source(tag, n, padded),
            )
        })
        .collect();
    let subject = format!("src/{tag}.ts");
    let source = wiring_source(tag, plugins);
    modules.push((subject.clone(), source.clone()));
    (Fixture::of(&modules, &subject), source)
}

/// The one part prompt that lists the outer registration.
fn outer_registration_part<'a>(dumps: &'a Dumps, source: &str) -> &'a String {
    let outer_line = source
        .lines()
        .position(|line| line.contains("server.register("))
        .unwrap() as i64
        + 1;
    // A part states a candidate's line in its own numbers.
    let outer: Vec<&String> = dumps
        .parts
        .iter()
        .filter(|(label, prompt)| {
            listed_in(prompt)
                .iter()
                .any(|c| dumps.terms[*label].shown.file_line(c.line) == Some(outer_line as u32))
        })
        .map(|(_, prompt)| prompt)
        .collect();
    let [prompt] = outer[..] else {
        panic!("one part lists the outer registration: {}", outer.len());
    };
    prompt
}

/// The plugin modules whose registration lines a part prompt shows, sorted.
fn registered_on_shown_lines(prompt: &str) -> Vec<String> {
    let mut modules: Vec<String> = file_section(prompt)
        .fence
        .iter()
        .filter(|line| line_number(line).is_some())
        .filter_map(|line| {
            let rest = line.split_once("api.register(plugin")?.1;
            rest.split(')').next()?.parse::<usize>().ok()
        })
        .map(|n| format!("src/plugins/plugin{n}.ts"))
        .collect();
    modules.sort();
    modules
}

/// carrick#2486: a registration whose inline callback holds the rest of the
/// file names, inside its call, every module the callback registers. The
/// part that lists it is over the line with all of them, so it sheds the ones
/// named only on lines it does not show, and the file is asked in parts under
/// the line rather than lost.
#[tokio::test]
#[serial]
async fn a_registration_whose_callback_holds_the_file_carries_only_the_modules_its_part_shows() {
    offline();
    let (fx, source) = wiring_fixture("wiring", PLUGINS, true);

    let (dumps, sent) = fx.dumped_run().await;
    assert_eq!(lost_reason(&fx.table), None, "the file is asked, not lost");
    assert!(dumps.whole.is_none(), "the file is over the line");
    assert_eq!(sent, PLUGINS + dumps.parts.len());
    for (label, prompt) in &dumps.parts {
        assert!(
            prompt.len() <= WHOLE_PROMPT_MAX_BYTES,
            "part {label} is {} bytes",
            prompt.len()
        );
    }
    let listed = listed_in_parts(&dumps);
    assert_eq!(
        listed.len(),
        PLUGINS + 1,
        "every registration is asked once"
    );

    // The part that lists the outer registration attaches the plugins its
    // shown lines register, and no other.
    let prompt = outer_registration_part(&dumps, &source);
    let expected = registered_on_shown_lines(prompt);
    assert!(!expected.is_empty() && expected.len() < PLUGINS);
    assert_eq!(attached_modules(prompt), expected);
}

/// carrick#2486: a part under the line keeps every module its candidates
/// name, those named only on lines it does not show included. Only a part
/// over the line sheds them, so a prompt that fitted before is unchanged.
#[tokio::test]
#[serial]
async fn a_part_under_the_line_keeps_every_module_its_candidates_name() {
    offline();
    let tag = "smallwiring";
    let (fx, source) = wiring_fixture(tag, SMALL_PLUGINS, false);
    // Small enough to be asked whole: the cloud's verdict that the answer was
    // cut is what asks it in parts.
    let dump = tempfile::tempdir().unwrap();
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    dump_into(Some(dump.path()));
    fx.analyze().await;
    dump_into(None);
    assert_eq!(lost_reason(&fx.table), None);
    let dumps = fx.dumps(dump.path());
    assert!(dumps.whole.is_none(), "the whole request was cut");

    let prompt = outer_registration_part(&dumps, &source);
    assert!(prompt.len() <= WHOLE_PROMPT_MAX_BYTES);
    let shown = registered_on_shown_lines(prompt);
    assert!(
        !shown.is_empty() && shown.len() < SMALL_PLUGINS,
        "the outer registration's callback runs past its part: {} shown",
        shown.len()
    );
    let mut every: Vec<String> = (0..SMALL_PLUGINS)
        .map(|n| format!("src/plugins/plugin{n}.ts"))
        .collect();
    every.sort();
    assert_eq!(attached_modules(prompt), every);
}

/// `count` interfaces, each a declaration that runs nothing.
fn interfaces(count: usize) -> String {
    (0..count)
        .map(|i| {
            format!("export interface OnlyATypeNumber{i:04} {{ value: string; other: number }}\n")
        })
        .collect()
}

/// carrick#2309: a window that holds no candidate and only declarations that
/// run nothing is not sent.
#[tokio::test]
#[serial]
async fn a_window_of_types_only_is_not_sent() {
    offline();
    let tag = "typesonly";
    let source = requests_after(tag, &interfaces(4_000), 4);
    let subject = format!("src/{tag}.ts");
    let fx = Fixture::of(&[(subject.clone(), source)], &subject);
    let (dumps, _) = fx.dumped_run().await;
    assert!(dumps.whole.is_none(), "the file is over the line");
    assert!(!dumps.parts.is_empty(), "the requests are asked");
    assert!(
        dumps
            .parts
            .values()
            .all(|prompt| !prompt.contains("OnlyATypeNumber0100 {")),
        "the windows of types alone are not sent"
    );
    assert_eq!(listed_in_parts(&dumps).len(), 4);
}

/// carrick#2309: beside a GraphQL section a type can be the thing located, so
/// the same windows of types are sent.
#[tokio::test]
#[serial]
async fn a_window_of_types_only_is_sent_beside_a_graphql_section() {
    offline();
    let tag = "typesgraphql";
    let source = requests_after(tag, &interfaces(4_000), 4);
    let subject = format!("src/{tag}.ts");
    let fx = Fixture::of(&[(subject.clone(), source)], &subject);
    let consumer = GraphqlConsumerHints {
        lines: vec!["query|typesGraphqlField @ src/elsewhere.ts".to_string()],
        files: Default::default(),
    };
    let dump = tempfile::tempdir().unwrap();
    dump_into(Some(dump.path()));
    fx.analyze_with(&HashMap::new(), &consumer).await;
    dump_into(None);
    let dumps = fx.dumps(dump.path());
    assert!(dumps.whole.is_none(), "the file is over the line");
    assert!(
        dumps
            .parts
            .values()
            .any(|prompt| prompt.contains("OnlyATypeNumber0100 {")),
        "the windows of types are sent"
    );
}

/// The cloud's final verdict that a request is too large to ask
/// (carrick-cloud#2267), stating `details`.
fn too_large(details: Value) -> String {
    json!({
        "success": false,
        "error": {
            "code": "prompt_too_large",
            "message": "stated by the test",
            "retriable": false,
            "details": details,
        },
    })
    .to_string()
}

/// carrick#2310: a file under the scanner's line whose whole request the
/// cloud refuses as too large is asked in parts under the limit it states.
#[tokio::test]
#[serial]
async fn a_whole_request_refused_for_size_is_asked_in_parts() {
    offline();
    let tag = "refusedwhole";
    let shape = shapes::flat_module(tag, 150);
    let subject = format!("src/{tag}_client.ts");
    let fx = Fixture::of(&[(subject.clone(), shape.source.clone())], &subject);
    let whole = fx.whole_prompt().await;
    assert!(
        whole.len() < WHOLE_PROMPT_MAX_BYTES,
        "a whole prompt under the scanner's line: {}",
        whole.len()
    );

    // Refused stating no limit, the file is cut into its windows.
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &too_large(json!({})));
    let (windows, _) = fx.dumped_run().await;
    let largest = windows.parts.values().map(String::len).max().unwrap();

    // Refused stating half the largest window's prompt, no request is over it.
    let stated = largest / 2;
    agent_service::inject_mock_envelope(
        ROUTE,
        &fx.whole_marker(),
        1,
        &too_large(json!({ "max_prompt_bytes": stated, "user_message_bytes": whole.len() })),
    );
    let (dumps, sent) = fx.dumped_run().await;
    assert!(
        dumps.whole.is_none(),
        "the refused request is not sent again"
    );
    assert_eq!(
        sent,
        1 + dumps.parts.len(),
        "the refused request and the parts"
    );
    for (label, prompt) in &dumps.parts {
        assert!(
            prompt.len() <= stated,
            "part {label}: {} bytes over the stated {stated}",
            prompt.len()
        );
    }
    assert_eq!(listed_in_parts(&dumps).len(), 150);
    assert_eq!(lost_reason(&fx.table), None, "the file's rows are stored");
}

/// carrick#2310: a limit no part could meet, stated by a misconfigured cloud,
/// is read as no limit. The refused request is cut into its windows and the
/// file's rows are stored, rather than every part being cut down to one item
/// over the line and the file lost for good.
#[tokio::test]
#[serial]
async fn a_stated_limit_no_part_could_meet_is_ignored() {
    offline();
    let tag = "tinylimit";
    let shape = shapes::flat_module(tag, 150);
    let subject = format!("src/{tag}_client.ts");
    let fx = Fixture::of(&[(subject.clone(), shape.source.clone())], &subject);
    let whole = fx.whole_prompt().await;
    agent_service::inject_mock_envelope(
        ROUTE,
        &fx.whole_marker(),
        1,
        &too_large(json!({ "max_prompt_bytes": 1000, "user_message_bytes": whole.len() })),
    );
    let (dumps, sent) = fx.dumped_run().await;
    assert!(
        dumps.whole.is_none(),
        "the refused request is not sent again"
    );
    assert!(dumps.parts.len() > 1, "{} part(s)", dumps.parts.len());
    assert_eq!(sent, 1 + dumps.parts.len());
    assert_eq!(listed_in_parts(&dumps).len(), 150);
    assert_eq!(lost_reason(&fx.table), None, "the file's rows are stored");
}

/// carrick#2310: a part the cloud refuses as too large is halved by its
/// bytes and its halves asked; the refused part is never sent again.
#[tokio::test]
#[serial]
async fn a_refused_part_is_halved() {
    offline();
    let tag = "refusedpart";
    let shape = shapes::flat_module(tag, 400);
    let subject = format!("src/{tag}_client.ts");
    let fx = Fixture::of(&[(subject.clone(), shape.source.clone())], &subject);
    let (first, _) = fx.dumped_run().await;
    let labels: Vec<String> = first.parts.keys().cloned().collect();
    let refused = &labels[1];
    // The refused part's first candidate, named by the text only it carries
    // (a part's own ids repeat across parts).
    let refused_first = first.terms[refused].file_id("c1").unwrap().to_string();
    let position = listed_in_parts(&first)
        .iter()
        .position(|candidate| candidate.id == refused_first)
        .unwrap();

    agent_service::inject_mock_envelope(
        ROUTE,
        &flat_module_marker(tag, position),
        1,
        &too_large(json!({})),
    );
    let (dumps, sent) = fx.dumped_run().await;
    assert_eq!(
        sent,
        labels.len() + 2,
        "every part, then the refused one's halves"
    );
    assert!(
        !dumps.parts.contains_key(refused),
        "the refused part is not sent again"
    );
    assert_eq!(dumps.parts.len(), labels.len() + 1);
    assert_eq!(
        listed_in_parts(&dumps).len(),
        400,
        "every candidate is asked"
    );
    assert_eq!(lost_reason(&fx.table), None);
}

/// carrick#2310: a window of one item the cloud refuses as too large cannot
/// be cut again, and the file is named under the reason step 2 introduced.
#[tokio::test]
#[serial]
async fn a_single_item_refused_for_size_loses_the_file_by_name() {
    offline();
    let tag = "refusedone";
    // One line, so the file is one item however far it is opened.
    let source = format!(
        "declare const client: any; export function readOne() {{ return client.get({{ url: \"/{tag}/items/0\" }}); }}\n"
    );
    let subject = format!("src/{tag}.ts");
    let fx = Fixture::of(&[(subject.clone(), source)], &subject);
    assert_eq!(fx.listed().await.len(), 1);

    // Every request about the file is refused.
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 3, &too_large(json!({})));
    let before = requests();
    let result = fx.analyze().await;
    let sent = requests() - before;
    assert!((1..=2).contains(&sent), "{sent} request(s)");
    assert_eq!(lost_reason(&fx.table).as_deref(), Some(FILE_TOO_LARGE_CODE));
    assert!(!result.raw_model_results.contains_key(&fx.table));
}

/// carrick#2310: a request refused for size is never sent again as it was.
#[tokio::test]
#[serial]
async fn a_request_refused_for_size_is_never_sent_again() {
    offline();
    let fx = Fixture::table("refusedonce");
    let whole = fx.whole_prompt().await;
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &too_large(json!({})));
    let (dumps, sent) = fx.dumped_run().await;
    assert!(dumps.whole.is_none(), "the whole request is not sent again");
    assert!(!dumps.parts.is_empty());
    assert!(dumps.parts.values().all(|part| part.len() < whole.len()));
    assert_eq!(sent, HANDLER_MODULES + 1 + dumps.parts.len());
    assert_eq!(lost_reason(&fx.table), None);
}

/// carrick#2308: a row that carries no candidate id is stored once however
/// many parts state it, and the one kept is from the part whose window holds
/// its line. A row only one part states is kept wherever its line is.
#[tokio::test]
#[serial]
async fn rows_with_no_candidate_are_kept_once() {
    offline();
    let tag = "keptonce";
    let fx = Fixture::table(tag);
    let listed = fx.listed().await;
    let terms = part_terms(&fx, &[(fx.whole_marker(), cut())]).await;
    // A part states a line in its own numbers.
    let mount = |part: &PartTerms, line: u32, child: &str, seen_by: &str| {
        json!({
            "line_number": part.shown.prompt_line(line).expect("the part shows the line"),
            "parent_node": "table",
            "child_node": child,
            "mount_path": format!("/{child}"),
            "import_source": null,
            "pattern_matched": seen_by,
        })
    };
    // Line 6 is in the first part's window and shown beside the window of the
    // other two. Every part states the mount on it; the last part alone states
    // one on line 5, outside its own window.
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    let seen_by = ["the first part", "the middle part", "the last part"];
    for ((marker, range), seen_by) in part_markers(tag).iter().zip(PART_RANGES).zip(seen_by) {
        let part = terms_holding(&terms, &listed[range.0].id);
        let mut mounts = vec![mount(part, 6, "shared", seen_by)];
        if range == PART_RANGES[2] {
            mounts.push(mount(part, 5, "far", seen_by));
        }
        let candidate = Listed {
            id: part.local_id(&listed[range.0].id).unwrap().to_string(),
            line: part
                .shown
                .prompt_line(listed[range.0].line as u32)
                .unwrap()
                .into(),
            ..listed[range.0].clone()
        };
        let answer = json!({
            "mounts": mounts,
            "endpoints": [endpoint_row(&candidate, range.0)],
            "data_calls": [],
        });
        agent_service::inject_mock_answer(ROUTE, marker, 1, &answer.to_string());
    }
    assert!((PART_WINDOWS[0].0..=PART_WINDOWS[0].1).contains(&6));
    assert!(!(PART_WINDOWS[1].0..=PART_WINDOWS[1].1).contains(&6));

    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(requests() - before, HANDLER_MODULES + 1 + 3);
    let stored = &result.raw_model_results[&fx.table];
    let mounts: Vec<(i32, &str, &str)> = stored
        .mounts
        .iter()
        .map(|row| {
            (
                row.line_number,
                row.child_node.as_str(),
                row.pattern_matched.as_str(),
            )
        })
        .collect();
    assert_eq!(
        mounts,
        [(6, "shared", "the first part"), (5, "far", "the last part")]
    );
    assert_eq!(stored.endpoints.len(), 3, "one route row a part");
}

// ---------------------------------------------------------------------------
// A part's prompt does not move with an edit outside it (carrick#2794)
// ---------------------------------------------------------------------------

/// The statements the edit tests insert, and what names them in a source.
const INSERTED: &str = "inserted";

/// The lines of `source` that hold an inserted statement.
fn inserted_lines(source: &str) -> Vec<u32> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(INSERTED))
        .map(|(index, _)| index as u32 + 1)
        .collect()
}

/// `source` with two statements that raise no candidate inserted after the
/// first line that holds `after`.
fn with_two_statements_after(source: &str, after: &str, indent: &str) -> String {
    let mut out = String::new();
    let mut done = false;
    for line in source.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.contains(after) {
            writeln!(out, "{indent}const {INSERTED}A = 1;").unwrap();
            writeln!(out, "{indent}const {INSERTED}B = 2;").unwrap();
            done = true;
        }
    }
    assert!(done, "{after} is in the source");
    out
}

/// A route table written as one exported function: its registrations are the
/// statements of its body, so a part shows the function's opening and closing
/// lines beside its window.
fn function_table_source(tag: &str) -> String {
    let mut out = String::new();
    out.push_str("declare function makeTable(): any;\n");
    out.push_str("export function register(): void {\n");
    out.push_str("  const table = makeTable();\n");
    out.push_str("  const local = (req: any, res: any) => {};\n");
    for i in 0..50 {
        writeln!(out, "  table.get(\"/{tag}/r/{i}\", local);").unwrap();
    }
    out.push_str("}\n");
    out
}

/// Both scans of a table, before and after an edit, with the whole request
/// cut so that parts follow the candidate limit and not the bytes: what each
/// sent for the subject.
async fn parts_before_and_after(before: &Fixture, after: &Fixture) -> (Dumps, Dumps) {
    let cut_at_ten = cut_stating(json!(10));
    let mut both = Vec::new();
    for fx in [before, after] {
        agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut_at_ten);
        both.push(fx.dumped_run().await.0);
    }
    let after = both.pop().unwrap();
    (both.pop().unwrap(), after)
}

/// A part's window, read off its label.
fn window_of(label: &str) -> (u32, u32) {
    let (first, last) = label.split_once('-').expect("a part label");
    (first.parse().unwrap(), last.parse().unwrap())
}

/// Every part of `after` whose window holds no line of the edit is a part
/// `before` sent, byte for byte; the part that holds it is a new prompt.
fn assert_only_the_part_that_holds_the_edit_differs(before: &Dumps, after: &Dumps, edited: &[u32]) {
    assert_eq!(
        before.parts.len(),
        after.parts.len(),
        "the edit moves no boundary between parts"
    );
    assert!(after.parts.len() > 3, "parts: {:?}", after.parts.keys());
    let sent_before: std::collections::HashSet<&String> = before.parts.values().collect();
    let mut changed = Vec::new();
    for (label, prompt) in &after.parts {
        let (first, last) = window_of(label);
        let holds_the_edit = edited.iter().any(|line| (first..=last).contains(line));
        assert_eq!(
            !sent_before.contains(prompt),
            holds_the_edit,
            "part {label} (window {first} to {last}, edit on {edited:?})"
        );
        if holds_the_edit {
            changed.push(label);
        }
    }
    assert_eq!(changed.len(), 1, "one part holds the edit: {changed:?}");
}

/// carrick#2794: a part's prompt states nothing about where in the file it
/// is, so an edit above or below it leaves its bytes alone, and the cloud's
/// cache answers it for nothing. A file whose parts are cut between its
/// statements and one whose parts are cut out of a function body.
#[tokio::test]
#[serial]
async fn an_edit_outside_a_part_leaves_its_prompt_the_same_bytes() {
    offline();

    // (a) Lines inserted among the declarations, inside the first part's
    // window: every registration below moves down two lines, and so does the
    // byte offset of each.
    let tag = "editedtable";
    let source = table_source(tag);
    let edited_source = with_two_statements_after(&source, "const local =", "");
    let before = Fixture::table_from(tag, source);
    let after = Fixture::table_from(tag, edited_source.clone());
    let (sent_before, sent_after) = parts_before_and_after(&before, &after).await;
    assert_only_the_part_that_holds_the_edit_differs(
        &sent_before,
        &sent_after,
        &inserted_lines(&edited_source),
    );

    // (b) The same registrations inside one function, and the lines inserted
    // inside the last part's window: the function's closing line moves, and
    // every part but the first shows it.
    let tag = "editedfunction";
    let source = function_table_source(tag);
    let edited_source =
        with_two_statements_after(&source, "table.get(\"/editedfunction/r/49\"", "  ");
    let before = Fixture::lone_table(tag, source);
    let after = Fixture::lone_table(tag, edited_source.clone());
    let (sent_before, sent_after) = parts_before_and_after(&before, &after).await;
    assert_only_the_part_that_holds_the_edit_differs(
        &sent_before,
        &sent_after,
        &inserted_lines(&edited_source),
    );
}

/// carrick#2794: the text a part's first candidate is named by in a request,
/// as the serialized request body holds it: the `[path: ...]` of its hint.
fn first_candidate_marker(prompt: &str) -> String {
    let hint = section(prompt, "CANDIDATE TARGETS")
        .lines()
        .find(|line| line.starts_with("- Candidate "))
        .expect("a part that lists a candidate");
    let start = hint.find("[path: ").expect("a hint names a path");
    let path = &hint[start..=start + hint[start..].find(']').unwrap()];
    path.replace('"', "\\\"")
}

/// carrick#2794: a part answer is read as the part states it. The cloud
/// returns the stored answer of an unchanged part for the same bytes, whatever
/// scan asked first, so that answer must mean the same lines and candidates
/// in the scan that receives it.
#[tokio::test]
#[serial]
async fn a_replayed_part_answer_lands_on_this_scans_lines_and_candidates() {
    offline();
    let tag = "replayed";
    let source = table_source(tag);
    let edited_source = with_two_statements_after(&source, "const local =", "");
    let before = Fixture::table_from(tag, source);
    let after = Fixture::table_from(tag, edited_source.clone());
    let listed_before = before.listed().await;
    let listed_after = after.listed().await;
    assert_the_table_lists_what_the_tests_assume(&listed_after);
    let whole_before = whole_answer(&listed_before);
    let whole_after = whole_answer(&listed_after);
    assert_eq!(
        listed_after[LOCAL_SINGLES].line,
        listed_before[LOCAL_SINGLES].line + 2,
        "the edit moved the registrations down"
    );

    // The first scan: each part is answered in what its prompt shows.
    let cut_at_ten = cut_stating(json!(10));
    let refusals = [(before.whole_marker(), cut_at_ten.clone())];
    let first = part_dumps(&before, &refusals).await;
    // The second scan before any answer, to know which of its parts the
    // first scan already asked.
    let refusals_after = [(after.whole_marker(), cut_at_ten.clone())];
    let second = part_dumps(&after, &refusals_after).await;

    agent_service::inject_mock_envelope(ROUTE, &after.whole_marker(), 1, &cut_at_ten);
    let mut replayed = 0;
    for (label, prompt) in &second.parts {
        let asked_before = first.parts.iter().find(|(_, earlier)| *earlier == prompt);
        // An unchanged part gets the answer the first scan's part got, which
        // is what the cloud hands back for the same bytes. The part that holds
        // the edit is asked afresh and answers in its own terms.
        let answer = match asked_before {
            Some((earlier, _)) => {
                replayed += 1;
                in_part_terms(&whole_before, &first.terms[earlier])
            }
            None => in_part_terms(&whole_after, &second.terms[label]),
        };
        agent_service::inject_mock_answer(
            ROUTE,
            &first_candidate_marker(prompt),
            1,
            &answer.to_string(),
        );
    }
    assert!(
        replayed + 1 == second.parts.len() && replayed > 3,
        "every part but the one that holds the edit is replayed: {replayed} of {}",
        second.parts.len()
    );

    let result = after.analyze().await;
    let stored = &result.raw_model_results[&after.table];
    assert_eq!(
        bytes(stored),
        stored_form(&whole_after),
        "the replayed answers carry this scan's candidate ids and lines"
    );
    assert_eq!(lost_reason(&after.table), None);
}

/// A $0 helper, run by hand: write the four generated shapes to disk, each as
/// a tree a scan can be pointed at.
///
/// ```text
/// CARRICK_SHAPES_OUT=<fresh dir> \
///   cargo test --test ask_in_parts_test write_the_generated_shapes -- --ignored
/// ```
///
/// Each lands at `<out>/<shape>/src/<shape>.ts` beside a `package.json`. The
/// probe below cuts a named file of such a tree, and two builds' whole
/// prompts for one are compared by dumping both (`CARRICK_EVAL_DUMP_DIR`).
///
/// The route array's tree also holds `expected.json`: each route's method,
/// path and handler, which is the answer its merged parts are checked against.
#[test]
#[ignore = "a helper run by hand: it writes to a directory named in the environment"]
fn write_the_generated_shapes() {
    const ROUTES: usize = 400;
    let out = PathBuf::from(std::env::var("CARRICK_SHAPES_OUT").expect("CARRICK_SHAPES_OUT"));
    for (name, shape) in shapes::every_shape("generated", ROUTES) {
        let root = out.join(name);
        std::fs::create_dir_all(root.join("src")).unwrap();
        if name == "route_array" {
            let expected: Vec<Value> = (0..ROUTES)
                .map(|i| {
                    json!({
                        "method": "GET",
                        "path": format!("/generated/routes/{i}"),
                        "handler": shapes::route_array_handler(i),
                    })
                })
                .collect();
            std::fs::write(
                root.join("expected.json"),
                serde_json::to_string_pretty(&expected).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
        std::fs::write(root.join(format!("src/{name}.ts")), &shape.source).unwrap();
        println!(
            "{name}: {} bytes, {} lines",
            shape.source.len(),
            shape.source.lines().count()
        );
    }
}

/// The partition is the same whichever build asks: the limit is this number.
#[test]
fn a_part_holds_at_most_thirty_two_candidates() {
    assert_eq!(MAX_PART_CANDIDATES, 32);
    assert_eq!((PART_WINDOW_BYTES, PART_CONTEXT_BYTES), (64_000, 16_000));
    // A hundred one-request functions, far under the window's bytes: the
    // candidate limit is what cuts them.
    let shape = shapes::flat_module("thirtytwo", 100);
    let listed: Vec<ListedCandidate> = shape
        .starts
        .iter()
        .map(|start| {
            let start = *start as u32 + 1;
            ListedCandidate {
                candidate_id: format!("span:{start}-{}", start + 8),
                span_start: start,
                span_end: start + 8,
            }
        })
        .collect();
    let sizes: Vec<usize> = source_parts(Path::new("src/flat.ts"), &shape.source, &listed, &LIMITS)
        .expect("the module parses")
        .iter()
        .map(|part| part.candidates.len())
        .collect();
    assert_eq!(sizes, [32, 32, 32, 4]);
}

/// A $0 probe, run by hand: make the mock cut the whole answer of named files
/// of a tree on disk, so their part prompts land in a dump directory.
///
/// ```text
/// CARRICK_PARTS_PROBE_TREE=<tree> \
/// CARRICK_PARTS_PROBE_FILES=<path as the prompt names it>[,<path>...] \
/// CARRICK_PARTS_PROBE_OUT=<fresh dir> \
///   cargo test --test ask_in_parts_test probe_the_part_prompts -- --ignored --nocapture
/// ```
///
/// Nothing leaves the machine: the scan runs under `CARRICK_MOCK_ALL`. The
/// part prompts land in `<out>/parts`, beside every other file's whole
/// prompt. It prints, for each named file, its parts with their candidates,
/// attached modules and prompt bytes, and fails unless every candidate of the
/// file is listed by exactly one part.
///
/// The candidates are read off the file's whole prompt, which a first pass
/// dumps into `<out>/whole`. `CARRICK_PARTS_PROBE_WHOLE=<dir>` names a
/// directory that already holds the tree's whole prompts and skips that pass.
#[tokio::test]
#[serial]
#[ignore = "a probe run by hand over a tree named in the environment"]
async fn probe_the_part_prompts_of_named_files() {
    let tree = std::env::var("CARRICK_PARTS_PROBE_TREE").expect("CARRICK_PARTS_PROBE_TREE");
    let named = std::env::var("CARRICK_PARTS_PROBE_FILES").expect("CARRICK_PARTS_PROBE_FILES");
    let dump =
        PathBuf::from(std::env::var("CARRICK_PARTS_PROBE_OUT").expect("CARRICK_PARTS_PROBE_OUT"));
    let named: Vec<&str> = named.split(',').filter(|path| !path.is_empty()).collect();
    // SAFETY: `#[serial]`, as in `offline`.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::set_var("CARRICK_ALLOW_UNPREPARED", "1");
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
        for ci in ["GITHUB_REPOSITORY", "GITHUB_ACTIONS", "CI"] {
            std::env::remove_var(ci);
        }
    }

    // The whole prompts: an earlier run's, or a first pass in which every
    // file answers.
    let whole_dump = match std::env::var("CARRICK_PARTS_PROBE_WHOLE") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let whole_dump = dump.join("whole");
            dump_into(Some(&whole_dump));
            carrick::engine::run_analysis_engine_with_sidecar(
                carrick::cloud_storage::MockStorage::new(),
                &tree,
                None,
                true,
            )
            .await
            .expect("the whole pass");
            whole_dump
        }
    };

    // The same scan with the named files' whole answers cut.
    for path in &named {
        agent_service::inject_mock_envelope(
            ROUTE,
            &format!("### FILE CONTENT (Path: {path})"),
            1,
            &cut(),
        );
    }
    let parts_dump = dump.join("parts");
    dump_into(Some(&parts_dump));
    let before = requests();
    carrick::engine::run_analysis_engine_with_sidecar(
        carrick::cloud_storage::MockStorage::new(),
        &tree,
        None,
        true,
    )
    .await
    .expect("the pass with cut answers");
    dump_into(None);
    println!(
        "analyze-file requests in the pass with cut answers: {}",
        requests() - before
    );

    for path in named {
        let stem = path.replace('/', "_");
        let whole: Value = serde_json::from_str(
            &std::fs::read_to_string(whole_dump.join(format!("{stem}.json")))
                .unwrap_or_else(|e| panic!("{path}: no whole prompt in the first pass ({e})")),
        )
        .unwrap();
        let whole = whole["request_user_message"].as_str().unwrap();
        let candidates: Vec<String> = listed_in(whole).into_iter().map(|c| c.id).collect();

        let mut parts: Vec<(String, String)> = std::fs::read_dir(&parts_dump)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name()?.to_string_lossy().to_string();
                let label = name
                    .strip_prefix(&format!("{stem}.part-"))?
                    .strip_suffix(".json")?
                    .to_string();
                let payload: Value =
                    serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
                Some((label, payload["request_user_message"].as_str()?.to_string()))
            })
            .collect();
        parts.sort();

        let mut listed_by: BTreeMap<&str, usize> =
            candidates.iter().map(|id| (id.as_str(), 0)).collect();
        println!(
            "{path}: {} candidates, whole prompt {} bytes, {} parts",
            candidates.len(),
            whole.len(),
            parts.len()
        );
        for (label, prompt) in &parts {
            let ids = listed_in(prompt);
            println!(
                "  part {label}: {} candidates, {} modules, {} bytes",
                ids.len(),
                attached_modules(prompt).len(),
                prompt.len()
            );
            for candidate in ids {
                *listed_by.get_mut(candidate.id.as_str()).unwrap_or_else(|| {
                    panic!(
                        "{path}: part {label} lists {} which the whole prompt does not",
                        candidate.id
                    )
                }) += 1;
            }
        }
        let astray: Vec<(&str, usize)> = listed_by
            .into_iter()
            .filter(|(_, times)| *times != 1)
            .collect();
        assert!(
            astray.is_empty(),
            "{path}: candidates not listed by exactly one part: {astray:?}"
        );
        println!("  every candidate is listed by exactly one part");
    }
}
