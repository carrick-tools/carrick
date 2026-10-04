//! A file whose answer cannot fit one model response is asked in parts
//! (carrick#1898).
//!
//! The scanner asks about a file in one request. When the cloud answers that
//! request with its final verdict that the answer was cut (`output_truncated`,
//! not retriable), the file's candidates are split into parts, each part is
//! asked on its own, and the answers are merged into the one answer the file
//! stores. These tests drive `FileOrchestrator::analyze_files` over a
//! synthetic route table with the model mocked, and every one of them counts
//! the requests the scan sent:
//!
//! - a cut whole answer becomes one stored answer that is the union of its
//!   parts, whatever order the parts are merged in;
//! - a stored whole answer split across the parts by candidate id merges back
//!   to itself exactly;
//! - a part that is cut is halved, and a single group that is cut loses the
//!   file by name;
//! - nothing else is split: a file that answers, a file lost for any other
//!   reason, a file with no candidates, and every file of a mocked scan.
//!
//! Its own test binary, `#[serial]`, and every count read as a delta: the
//! request counters, the injected answers and the scan-health registry are
//! process-globals. Each test names its fixture's routes after itself, so an
//! injected answer can only ever match that test's prompts.

use carrick::agent_service::{self, AgentService};
use carrick::agents::file_analyzer_agent::{FileAnalysisResult, FileAnalyzerAgent};
use carrick::agents::file_orchestrator::{
    FileCentricAnalysisResult, FileOrchestrator, ListedCandidate, MAX_PART_CANDIDATES,
    candidate_parts, merge_part_answers,
};
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use serde_json::{Value, json};
use serial_test::serial;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const ROUTE: &str = "/analyze-file";

/// Modules the table imports its handlers from. Each one makes a call the
/// scanner raises and exports a binding, so its source rides in the prompt of
/// a file a call in which reaches it (carrick#1928): the table's
/// registrations call the handlers they import.
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
/// lists them, and the position of each part's first group among the file's
/// groups (a chain is one group).
const PART_RANGES: [(usize, usize); 3] = [(0, 30), (30, 62), (62, 70)];
const PART_FIRST_GROUPS: [usize; 3] = [0, 30, 60];

fn handler_source(n: usize) -> String {
    format!("export function h{n}(req: any, res: any) {{\n  res.send({{ handled: {n} }});\n}}\n")
}

fn single(out: &mut String, tag: &str, i: usize, handler: &str) {
    writeln!(out, "table.get(\"/{tag}/r/{i}\", {handler});").unwrap();
}

/// A handler written at the registration that calls imported handler `n`:
/// the call is what reaches that handler's module. A handler handed over by
/// name is an argument, and reaches nothing.
fn calling(n: usize) -> String {
    format!("(req: any, res: any) => h{n}(req, res)")
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
    writeln!(
        out,
        "table.route(\"/{tag}/c/0\").get({}).post({});",
        calling(0),
        calling(1)
    )
    .unwrap();
    for i in LOCAL_SINGLES..LAST_PART_FIRST_SINGLE {
        single(&mut out, tag, i, &calling(i % HANDLER_MODULES));
    }
    for i in LAST_PART_FIRST_SINGLE..SINGLES {
        single(&mut out, tag, i, &calling(0));
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
        let mut modules: Vec<(String, String)> = (0..HANDLER_MODULES)
            .map(|n| (format!("src/handlers/h{n}.ts"), handler_source(n)))
            .collect();
        modules.push((format!("src/{tag}_table.ts"), table_source(tag)));
        Self::of(&modules, &format!("src/{tag}_table.ts"))
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
        FileOrchestrator::new(AgentService::new())
            .analyze_files(
                &self.files,
                &HashMap::new(),
                &ProtocolGuidance::from([(Protocol::Http, guidance())]),
                &detection(),
                &self.root,
                &self.root,
                &[],
                &Default::default(),
                &Default::default(),
                &carrick::url_normalizer::UrlNormalizer::default_permissive(),
                &carrick::workspace_resolver::WorkspaceIndex::build_with_aliases(&self.root, None),
                None,
                // No call graph: this entry attaches no module to any prompt.
                &Default::default(),
            )
            .await
            .expect("the analysis runs")
    }

    /// The whole scan, as the engine runs it: discovery resolves the calls
    /// that decide which modules a prompt carries (carrick#1928). Answers how
    /// the scan ended.
    async fn try_scan(&self) -> Result<(), String> {
        std::fs::write(
            self.root.join("package.json"),
            r#"{"name":"table","version":"1.0.0"}"#,
        )
        .unwrap();
        carrick::engine::run_analysis_engine_with_sidecar(
            carrick::cloud_storage::MockStorage::new(),
            self.root.to_str().unwrap(),
            None,
            true,
        )
        .await
        .map_err(|error| error.to_string())
    }

    /// [`Self::try_scan`], for a scan that loses no file.
    async fn scan(&self) {
        self.try_scan().await.expect("a mocked scan");
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
            }
        }
        Dumps { whole, parts }
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

    /// The same from a whole scan ([`Self::scan`]): the prompt as it reads
    /// with the modules the file's calls reach.
    async fn scanned_whole_prompt(&self) -> String {
        let dump = tempfile::tempdir().unwrap();
        dump_into(Some(dump.path()));
        self.scan().await;
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
}

/// One candidate as a prompt's CANDIDATE CONTEXT section states it.
#[derive(Debug, Clone)]
struct Listed {
    id: String,
    span_start: u32,
    span_end: u32,
    line: i64,
}

fn listed_in(prompt: &str) -> Vec<Listed> {
    section(prompt, "CANDIDATE CONTEXT")
        .lines()
        .filter(|line| line.starts_with("{\"candidate_id\""))
        .map(|line| {
            // The section's closing note follows the last object on its line.
            let value: Value = serde_json::Deserializer::from_str(line)
                .into_iter()
                .next()
                .expect("a candidate context line")
                .expect("a candidate context object");
            Listed {
                id: value["candidate_id"].as_str().unwrap().to_string(),
                span_start: value["span_start"].as_u64().unwrap() as u32,
                span_end: value["span_end"].as_u64().unwrap() as u32,
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

/// A prompt with its three per-part sections removed: what a part must hold
/// byte for byte as the whole prompt does.
fn outside_the_part_sections(prompt: &str) -> String {
    let mut out = prompt.to_string();
    for name in [
        "CANDIDATE TARGETS",
        "CANDIDATE CONTEXT",
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
    // The registry files a loss under the service being scanned, and keeps
    // it for the life of the process: a test that ran the engine may have
    // left a service entered, and a loss an earlier test recorded would read
    // to the engine as work this scan still owes.
    carrick::scan_health::enter_service(None);
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

/// What the hint line of single registration `i` holds and nothing else in a
/// request does, as it reads inside the serialized request body.
fn single_marker(tag: &str, i: usize) -> String {
    format!("[path: \\\"/{tag}/r/{i}\\\"]")
}

/// Why this run lost `path`, when it did.
fn lost_reason(path: &str) -> Option<String> {
    carrick::scan_health::unanalysed_files_for(None)
        .into_iter()
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
/// share of `whole`, rows reversed. Returns the answers injected, in part
/// order.
fn cut_whole_and_answer_parts(
    fx: &Fixture,
    tag: &str,
    whole: &Value,
    listed: &[Listed],
) -> Vec<Value> {
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    let answers: Vec<Value> = PART_RANGES
        .iter()
        .map(|range| reversed(projected(whole, listed, *range)))
        .collect();
    for (marker, answer) in part_markers(tag).iter().zip(&answers) {
        agent_service::inject_mock_answer(ROUTE, marker, 1, &answer.to_string());
    }
    answers
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
    cut_whole_and_answer_parts(&fx, tag, &whole, &listed);

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
    let answers = cut_whole_and_answer_parts(&fx, tag, &whole, &listed);

    let before = requests();
    let result = fx.analyze().await;
    assert_eq!(requests() - before, HANDLER_MODULES + 1 + 3);
    let stored = bytes(&result.raw_model_results[&fx.table]);

    // The same three answers, handed to the merge in every order.
    let candidates = as_listed_candidates(&listed);
    let parts: Vec<(usize, FileAnalysisResult)> = PART_FIRST_GROUPS
        .iter()
        .zip(&answers)
        .map(|(first_group, answer)| {
            let parsed = FileAnalyzerAgent::result_from_answer("part", &answer.to_string())
                .expect("a part answer parses");
            (*first_group, parsed)
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
        let arrived: Vec<(usize, FileAnalysisResult)> =
            order.iter().map(|part| parts[*part].clone()).collect();
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
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    for (marker, range) in part_markers(tag).iter().zip(PART_RANGES) {
        let part = projected(&whole, &listed, range);
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
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    agent_service::inject_mock_envelope(ROUTE, &single_marker(tag, LOCAL_SINGLES), 1, &cut());
    for (marker, range) in [
        (single_marker(tag, 0), PART_RANGES[0]),
        (single_marker(tag, LOCAL_SINGLES), front),
        (single_marker(tag, LOCAL_SINGLES + 14), back),
        (single_marker(tag, LAST_PART_FIRST_SINGLE), PART_RANGES[2]),
    ] {
        let part = reversed(projected(&whole, &listed, range));
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

#[tokio::test]
#[serial]
async fn a_part_prompt_is_the_whole_prompt_with_its_own_candidates_and_modules() {
    offline();
    let tag = "prompt";
    let fx = Fixture::table(tag);
    // Through the engine, both times: which modules a prompt carries is read
    // off the call graph discovery resolves.
    let whole = fx.scanned_whole_prompt().await;
    let listed = listed_in(&whole);
    assert_the_table_lists_what_the_tests_assume(&listed);
    let every_module: Vec<String> = (0..HANDLER_MODULES)
        .map(|n| format!("src/handlers/h{n}.ts"))
        .collect();
    assert_eq!(
        attached_modules(&whole),
        every_module,
        "the whole prompt carries every module the table calls into"
    );

    let dump = tempfile::tempdir().unwrap();
    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    dump_into(Some(dump.path()));
    let before = requests();
    fx.scan().await;
    dump_into(None);
    assert_eq!(requests() - before, HANDLER_MODULES + 1 + 3);

    let dumps = fx.dumps(dump.path());
    assert!(
        dumps.whole.is_none(),
        "a request that was cut has no answer to dump"
    );
    let parts: Vec<&String> = dumps.parts.values().collect();
    assert_eq!(parts.len(), 3, "part dumps: {:?}", dumps.parts.keys());

    // What the calls written inside each part's candidates reach: the first
    // part's handlers are the table's own, the middle part's call into all
    // four modules, the last part's into the first.
    let modules: [&[String]; 3] = [&[], &every_module, &every_module[..1]];
    for ((part, (from, to)), modules) in parts.iter().zip(PART_RANGES).zip(modules) {
        let ids: Vec<&str> = listed[from..to].iter().map(|c| c.id.as_str()).collect();
        let in_context: Vec<String> = listed_in(part).into_iter().map(|c| c.id).collect();
        assert_eq!(in_context, ids, "the part's candidate contexts");
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
            "a part that reaches no module carries no such section"
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
            "guidance, import table, file text and instructions are the whole prompt's"
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
async fn a_file_with_no_candidates_is_not_split() {
    offline();
    // A file that raises no candidate and is asked about because a call in it
    // reaches a module that makes one. The call graph says so, and that is
    // discovery's, so the file is scanned through the engine.
    let fx = Fixture::of(
        &[
            ("src/handlers/h0.ts".to_string(), handler_source(0)),
            (
                "src/nocandidates_uses.ts".to_string(),
                "import { h0 } from \"./handlers/h0\";\nexport const use = () => h0(1 as any, 2 as any);\n"
                    .to_string(),
            ),
        ],
        "src/nocandidates_uses.ts",
    );
    let whole = fx.scanned_whole_prompt().await;
    assert!(listed_in(&whole).is_empty(), "the file lists no candidate");

    agent_service::inject_mock_envelope(ROUTE, &fx.whole_marker(), 1, &cut());
    let before = requests();
    // How a scan that lost a file to a final verdict ends is not this test's
    // to say (carrick#1924): it reads the requests sent and the loss.
    let _ended = fx.try_scan().await;
    assert_eq!(requests() - before, 1 + 1, "one request a file");
    assert_eq!(lost_reason(&fx.table).as_deref(), Some("output_truncated"));
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

/// The partition is the same whichever build asks: the limit is this number.
#[test]
fn a_part_holds_at_most_thirty_two_candidates() {
    assert_eq!(MAX_PART_CANDIDATES, 32);
    let listed: Vec<ListedCandidate> = (0..100u32)
        .map(|i| ListedCandidate {
            candidate_id: format!("span:{}-{}", i * 10, i * 10 + 5),
            span_start: i * 10,
            span_end: i * 10 + 5,
        })
        .collect();
    let sizes: Vec<usize> = candidate_parts(&listed, MAX_PART_CANDIDATES)
        .iter()
        .map(|part| part.candidates().len())
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
