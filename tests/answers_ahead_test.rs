//! A service's file-reader answers asked while the scan analyses the service
//! before it (carrick#3131).
//!
//! The lane asks a service's files under the bytes it built, and the
//! service's analysis takes an answer only for the bytes it builds itself.
//! Every test here counts the reader's requests against a run with nothing
//! asked ahead: a file asked ahead is never asked again.
//!
//! - T1 and T2 drive `FileOrchestrator::analyze_files` over two route files,
//!   once as the lane and once as the analysis, with the model mocked.
//! - T3 scans `tests/fixtures/parallel-services` one service at a time, once
//!   on `main` and once more after two later services each change a file,
//!   with the reader's answers held 400 ms, with the lane on and off.
//!
//! Its own test binary, `#[serial]`: the request counters, the injected
//! answers, the scan-health registry and the environment are process-globals.

use async_trait::async_trait;
use carrick::agent_service::{self, AgentService};
use carrick::agents::asked_ahead::{AheadRole, AheadSlot};
use carrick::agents::file_orchestrator::{FileCentricAnalysisResult, FileOrchestrator};
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use carrick::engine::service_workers::{SERVICE_CONCURRENCY_ENV as WIDTH_ENV, take_schedules};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use serial_test::serial;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const ROUTE: &str = "/analyze-file";
const AHEAD_ENV: &str = "CARRICK_ANSWERS_AHEAD";

fn requests_to(route: &str) -> usize {
    agent_service::request_counts()
        .get(route)
        .copied()
        .unwrap_or(0)
}

fn guidance() -> FrameworkGuidance {
    FrameworkGuidance {
        mount_patterns: vec![],
        endpoint_patterns: vec![PatternExample {
            pattern: ".get(".to_string(),
            description: "a route registration".to_string(),
            framework: "generic".to_string(),
        }],
        middleware_patterns: vec![],
        data_fetching_patterns: vec![],
        triage_hints: String::new(),
        parsing_notes: String::new(),
        guidance_key: None,
    }
}

fn offline() {
    // SAFETY: every test in this binary is `#[serial]`, so no other thread
    // reads the environment while these are set.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
    }
    carrick::scan_health::forget_service_losses(None);
}

/// Files of one service under a fresh root (two route files unless built
/// otherwise), and the setup they are read under.
struct Routes {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    files: Vec<PathBuf>,
    guidance: FrameworkGuidance,
    detection: DetectionResult,
}

impl Routes {
    fn new(tag: &str) -> Self {
        Self::of(
            ["a", "b"]
                .into_iter()
                .map(|name| (name.to_string(), Self::source(tag, name)))
                .collect(),
        )
    }

    /// `src/<name>.ts` holding each source, read under [`guidance`].
    fn of(sources: Vec<(String, String)>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let mut files = Vec::new();
        for (name, source) in sources {
            let path = root.join(format!("src/{name}.ts"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, source).unwrap();
            files.push(path);
        }
        Self {
            _tmp: tmp,
            root,
            files,
            guidance: guidance(),
            detection: DetectionResult::default(),
        }
    }

    fn source(tag: &str, name: &str) -> String {
        format!(
            "declare function makeTable(): any;\nconst table = makeTable();\n\
             table.get(\"/{tag}/{name}\", (req: any, res: any) => res.send({{ ok: true }}));\n"
        )
    }

    /// What the reader's request about `name` carries and no other's does.
    fn marker(name: &str) -> String {
        format!("### FILE CONTENT (Path: src/{name}.ts)")
    }

    fn edit(&self, name: &str) {
        let path = self.root.join(format!("src/{name}.ts"));
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("// edited after the lane asked\n");
        std::fs::write(path, text).unwrap();
    }

    async fn analyze(&self, ahead: AheadRole) -> FileCentricAnalysisResult {
        FileOrchestrator::new(AgentService::new())
            .with_answers_ahead(ahead)
            .analyze_files(
                &self.files,
                &HashMap::new(),
                &ProtocolGuidance::from([(Protocol::Http, self.guidance.clone())]),
                &self.detection,
                &self.root,
                &self.root,
                &[],
                &Default::default(),
                &Default::default(),
                &carrick::url_normalizer::UrlNormalizer::default_permissive(),
            )
            .await
            .expect("the analysis runs")
    }

    /// What the analysis came to, and the reader's requests it sent.
    async fn analyze_counting(&self, ahead: AheadRole) -> (FileCentricAnalysisResult, usize) {
        let before = requests_to(ROUTE);
        let result = self.analyze(ahead).await;
        (result, requests_to(ROUTE) - before)
    }
}

fn lane(slot: &Arc<AheadSlot>) -> AheadRole {
    AheadRole::Lane {
        slot: slot.clone(),
        stop: Arc::new(AtomicBool::new(false)),
        log: tracing::Dispatch::none(),
    }
}

fn losses() -> usize {
    carrick::scan_health::service_losses(None).files
}

/// T1: an answer the lane asked for the bytes the analysis builds is the
/// analysis's answer. The reader is asked nothing more, and the answer is
/// counted as dispatched, exactly as a run with nothing asked ahead counts
/// it.
#[tokio::test]
#[serial]
async fn an_answer_asked_ahead_for_the_same_bytes_is_taken_and_counted_as_dispatched() {
    offline();
    let routes = Routes::new("t1");
    let (main, main_asks) = routes.analyze_counting(AheadRole::None).await;
    assert_eq!(main_asks, 2, "both files are asked");

    let slot = Arc::new(AheadSlot::default());
    let (_, lane_asks) = routes.analyze_counting(lane(&slot)).await;
    let held = slot.held().expect("the lane published");
    let attempted = carrick::scan_health::attempted_count();
    let (analysed, analysis_asks) = routes
        .analyze_counting(AheadRole::Analysis(slot.clone()))
        .await;

    assert_eq!(analysis_asks, 0, "no request reaches the reader");
    assert_eq!(lane_asks + analysis_asks, main_asks, "as many asks as main");
    assert_eq!(
        (held.asked(), held.taken(), held.unused()),
        (2, 2, 0),
        "asked 2, taken 2, unused 0"
    );
    assert_eq!(
        (
            analysed.stats.files_model_dispatched,
            analysed.stats.files_model_reused
        ),
        (
            main.stats.files_model_dispatched,
            main.stats.files_model_reused
        )
    );
    assert_eq!(
        carrick::scan_health::attempted_count() - attempted,
        main.stats.files_model_dispatched,
        "attempted as a run with nothing asked ahead"
    );
    let raw = |result: &FileCentricAnalysisResult| -> BTreeMap<String, String> {
        result
            .raw_model_results
            .iter()
            .map(|(file, answer)| (file.clone(), serde_json::to_string(answer).unwrap()))
            .collect()
    };
    assert_eq!(
        raw(&analysed),
        raw(&main),
        "the held answers are the answers"
    );
}

/// T2a: an answer asked for bytes the analysis no longer builds (the file
/// changed under the scan) is never taken: the analysis asks that file for
/// itself, and the lane's ask is counted unused.
#[tokio::test]
#[serial]
async fn an_answer_asked_ahead_for_other_bytes_is_never_taken() {
    offline();
    let routes = Routes::new("t2a");
    let (_, main_asks) = routes.analyze_counting(AheadRole::None).await;

    let slot = Arc::new(AheadSlot::default());
    let (_, lane_asks) = routes.analyze_counting(lane(&slot)).await;
    let held = slot.held().expect("the lane published");
    routes.edit("a");
    let (analysed, analysis_asks) = routes
        .analyze_counting(AheadRole::Analysis(slot.clone()))
        .await;

    assert_eq!(
        analysis_asks, 1,
        "the changed file is asked by the analysis"
    );
    assert_eq!((held.asked(), held.taken(), held.unused()), (2, 1, 1));
    assert_eq!(
        lane_asks + analysis_asks,
        main_asks + held.unused(),
        "as many asks as main, and the one asked for other bytes"
    );
    assert_eq!(analysed.stats.files_model_dispatched, 2);
}

/// T2b: an ask the lane made that failed is a miss: the analysis asks that
/// file once more, and no loss is recorded from the lane's failure.
#[tokio::test]
#[serial]
async fn a_failed_ask_ahead_is_asked_again_by_the_loop() {
    offline();
    let routes = Routes::new("t2b");
    let (_, main_asks) = routes.analyze_counting(AheadRole::None).await;

    let slot = Arc::new(AheadSlot::default());
    agent_service::inject_mock_failure(ROUTE, &Routes::marker("a"), 1);
    let (_, lane_asks) = routes.analyze_counting(lane(&slot)).await;
    assert_eq!(losses(), 0, "the lane records no loss");
    let held = slot.held().expect("the lane published");
    let (analysed, analysis_asks) = routes
        .analyze_counting(AheadRole::Analysis(slot.clone()))
        .await;

    assert_eq!(
        analysis_asks, 1,
        "the failed file is asked once by the loop"
    );
    assert_eq!((held.asked(), held.taken(), held.unused()), (2, 1, 0));
    assert_eq!(
        lane_asks + analysis_asks,
        main_asks + 1,
        "as many asks as main, and the one the lane's failure costs"
    );
    assert_eq!(losses(), 0, "the loop's ask answered: nothing is lost");
    assert!(
        analysed
            .raw_model_results
            .keys()
            .any(|file| file.ends_with("src/a.ts")),
        "the failed file has its answer"
    );
}

/// The review of carrick#3131: an answer the lane asked under other inputs
/// than the analysis builds with (here the setup's guidance, which every
/// prompt carries) is never taken. The analysis asks every file again, and
/// each of the lane's asks is counted unused.
#[tokio::test]
#[serial]
async fn an_answer_asked_ahead_under_other_guidance_is_never_taken() {
    offline();
    let mut routes = Routes::new("stale");
    let slot = Arc::new(AheadSlot::default());
    let (_, lane_asks) = routes.analyze_counting(lane(&slot)).await;
    let held = slot.held().expect("the lane published");
    routes.guidance.endpoint_patterns[0].description =
        "a route registration, as the analysis was told".to_string();
    let (analysed, analysis_asks) = routes
        .analyze_counting(AheadRole::Analysis(slot.clone()))
        .await;

    assert_eq!(lane_asks, 2);
    assert_eq!(analysis_asks, 2, "the analysis asks both files itself");
    assert_eq!((held.asked(), held.taken(), held.unused()), (2, 0, 2));
    assert_eq!(analysed.stats.files_model_dispatched, 2);
}

/// What the cloud says when a file's answer does not fit one response: the
/// analysis then asks the file again in parts.
fn cut() -> String {
    serde_json::json!({
        "success": false,
        "error": {
            "code": "output_truncated",
            "message": "stated by the test",
            "retriable": false,
        },
    })
    .to_string()
}

/// The review of carrick#3131: a lane ask is one request, sent once. The
/// lane asks through a reader that retries nothing, and a file whose answer
/// does not fit one response is not cut and asked again by the lane: the
/// analysis asks it with its own retries and cuts, so the lane costs at most
/// one request more per file than a run with nothing asked ahead.
#[tokio::test]
#[serial]
async fn a_lane_ask_is_one_request_and_never_cut_or_retried() {
    offline();
    let routes = Routes::new("once");
    let slot = Arc::new(AheadSlot::default());
    let as_lane = FileOrchestrator::new(AgentService::new()).with_answers_ahead(lane(&slot));
    assert_eq!(
        as_lane.ahead_retry_policy(),
        Some(agent_service::RetryPolicy::ONCE),
        "the lane's requests are never retried"
    );
    assert_eq!(
        FileOrchestrator::new(AgentService::new()).ahead_retry_policy(),
        None
    );

    agent_service::inject_mock_envelope(ROUTE, &Routes::marker("a"), 1, &cut());
    let (_, lane_asks) = routes.analyze_counting(lane(&slot)).await;
    assert_eq!(lane_asks, 2, "one request per file, the cut one included");
    let held = slot.held().expect("the lane published");
    let (analysed, analysis_asks) = routes
        .analyze_counting(AheadRole::Analysis(slot.clone()))
        .await;
    assert_eq!((held.asked(), held.taken(), held.unused()), (2, 1, 0));
    assert_eq!(analysis_asks, 1, "the analysis asks the cut file itself");
    assert!(
        analysed
            .raw_model_results
            .keys()
            .any(|file| file.ends_with("src/a.ts"))
    );
}

/// The review of carrick#3131: the lane makes none of the analysis's side
/// effects. It counts no file attempted, records no loss and no file read
/// for the service's timing line, even when an ask fails. The analysis, in
/// the same scope, does each.
#[tokio::test]
#[serial]
async fn the_lane_makes_none_of_the_analysis_s_side_effects() {
    offline();
    let routes = Routes::new("quiet");
    let service = Some("side-effects".to_string());
    let attempted = carrick::scan_health::attempted_count();
    let lost = carrick::scan_health::lost_file_count();
    let slot = Arc::new(AheadSlot::default());
    agent_service::inject_mock_failure(ROUTE, &Routes::marker("a"), 1);
    let read = carrick::current_service::asked_for(service.clone(), async {
        routes.analyze(lane(&slot)).await;
        carrick::scan_timing::files_of_current_service()
    })
    .await;
    assert_eq!(slot.held().expect("the lane published").asked(), 2);
    assert_eq!(read, None, "no files read for the service's timing");
    assert_eq!(
        carrick::scan_health::attempted_count(),
        attempted,
        "no file attempted"
    );
    assert_eq!(
        carrick::scan_health::lost_file_count(),
        lost,
        "no loss recorded"
    );

    let read = carrick::current_service::asked_for(service, async {
        routes
            .analyze(AheadRole::Analysis(Arc::new(AheadSlot::default())))
            .await;
        carrick::scan_timing::files_of_current_service()
    })
    .await;
    assert_eq!(read, Some(2), "the analysis counts the files it reads");
    assert_eq!(carrick::scan_health::attempted_count(), attempted + 2);
}

/// The review of carrick#3131: the lane is not for HTTP alone. A pub/sub
/// publisher, sent to the reader because it imports a messaging client the
/// setup names, is asked ahead and its answer taken.
#[tokio::test]
#[serial]
async fn a_pub_sub_publisher_asked_ahead_is_taken() {
    offline();
    let mut routes = Routes::of(vec![(
        "publisher".to_string(),
        "import { connect } from \"nats\";\n\
         const nc = await connect();\n\
         nc.publish(\"orders.created\", JSON.stringify({ id: 1 }));\n"
            .to_string(),
    )]);
    routes.detection.messaging_clients = vec!["nats".to_string()];
    let (main, main_asks) = routes.analyze_counting(AheadRole::None).await;
    assert_eq!(main_asks, 1, "the publisher is sent to the reader");

    let slot = Arc::new(AheadSlot::default());
    let (_, lane_asks) = routes.analyze_counting(lane(&slot)).await;
    let held = slot.held().expect("the lane published");
    let (analysed, analysis_asks) = routes
        .analyze_counting(AheadRole::Analysis(slot.clone()))
        .await;
    assert_eq!((lane_asks, analysis_asks), (1, 0));
    assert_eq!((held.asked(), held.taken(), held.unused()), (1, 1, 0));
    assert_eq!(
        analysed.stats.files_model_dispatched,
        main.stats.files_model_dispatched
    );
}

/// The review of carrick#3131: once nothing more is wanted for a service
/// (its analysis refused the lane), the lane sends no ask it has not sent
/// yet. One more file than the lane asks at once, each ask held a second:
/// the stop set while the first asks are in flight keeps the last unsent.
#[tokio::test]
#[serial]
async fn a_stopped_lane_sends_no_further_ask() {
    offline();
    let depth = carrick::agents::file_orchestrator::FILE_ANALYSIS_QUEUE_DEPTH;
    let routes = Routes::of(
        (0..=depth)
            .map(|at| {
                let name = format!("f{at}");
                let source = Routes::source("stopped", &name);
                (name, source)
            })
            .collect(),
    );
    let slot = Arc::new(AheadSlot::default());
    let stop = Arc::new(AtomicBool::new(false));
    let role = AheadRole::Lane {
        slot: slot.clone(),
        stop: stop.clone(),
        log: tracing::Dispatch::none(),
    };
    agent_service::set_mock_latency(ROUTE, Some(Duration::from_secs(1)));
    let before = requests_to(ROUTE);
    let stopping = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
    };
    let (_, ()) = tokio::join!(routes.analyze(role), stopping);
    agent_service::set_mock_latency(ROUTE, None);
    assert_eq!(
        requests_to(ROUTE) - before,
        depth,
        "the last ask is not sent"
    );
    assert_eq!(slot.held().expect("the lane published").asked(), depth);
}

// T3, the whole scan.

const FIXTURE: &str = "parallel-services";
/// How long each reader ask is held. Long beside a debug build's analysis
/// on a shared CI machine, so a phase that waited out an ask cannot pass for
/// one that did not.
const LATENCY: Duration = Duration::from_secs(2);

/// In-memory storage that hands each scan every blob uploaded before it, as
/// the cloud's download does.
#[derive(Default, Clone)]
struct Store {
    repos: Arc<Mutex<Vec<CloudRepoData>>>,
}

#[async_trait]
impl CloudStorage for Store {
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        _final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        self.repos.lock().unwrap().push(data.clone());
        Ok(UploadOutcome::default())
    }
    async fn index_landed(
        &self,
        data: &CloudRepoData,
        _written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError> {
        Ok(self.repos.lock().unwrap().iter().any(|stored| {
            stored.repo_name == data.repo_name
                && stored.service_name == data.service_name
                && stored.commit_hash == data.commit_hash
        }))
    }
    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        let repos = self
            .repos
            .lock()
            .unwrap()
            .iter()
            .map(|stored| {
                let bytes = serde_json::to_string(stored).expect("a stored copy serializes");
                serde_json::from_str(&bytes).expect("a stored copy reads back")
            })
            .collect();
        Ok((repos, HashMap::new()))
    }
    async fn upload_type_file(
        &self,
        _repo_name: &str,
        _file_name: &str,
        _content: &str,
    ) -> Result<(), StorageError> {
        Ok(())
    }
    async fn health_check(&self) -> Result<(), StorageError> {
        Ok(())
    }
    async fn upload_logs(&self, _repo: &str, _log_content: &str) -> Result<(), StorageError> {
        Ok(())
    }
    async fn post_pr_result(
        &self,
        _payload: &carrick::findings::PrResultPayload,
    ) -> Result<(), StorageError> {
        Ok(())
    }
    fn supports_multi_service(&self) -> bool {
        true
    }
}

impl Store {
    fn uploads(&self) -> usize {
        self.repos.lock().unwrap().len()
    }

    /// The blobs uploaded from `from` on, as the text the upload sends, keyed
    /// by service, with the fields that state the run rather than the
    /// service pinned.
    fn uploaded_text(&self, from: usize) -> BTreeMap<String, String> {
        self.repos.lock().unwrap()[from..]
            .iter()
            .map(|blob| {
                let mut blob = blob.clone();
                blob.last_updated = chrono::DateTime::UNIX_EPOCH;
                blob.memory_peaks = None;
                (
                    blob.service_name.clone().unwrap_or_default(),
                    serde_json::to_string(&blob).expect("the upload serializes"),
                )
            })
            .collect()
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// The real type sidecar, initialised on `repo`.
fn real_sidecar(repo: &Path) -> carrick::services::type_sidecar::TypeSidecar {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    assert!(
        entry.exists(),
        "build the sidecar first: cd src/sidecar && npm ci && npm run build"
    );
    let sidecar =
        carrick::services::type_sidecar::TypeSidecar::spawn(&entry).expect("the sidecar spawns");
    sidecar.start_init(repo, None);
    sidecar
        .wait_ready(Duration::from_secs(300))
        .expect("the sidecar initialises on the fixture");
    sidecar
}

/// Every line the scan logged, with when it was written: info, and each
/// reader prompt's fingerprint, as the run's log keeps them. One subscriber
/// for the process, so the services' own threads write to it too.
fn log() -> &'static Mutex<Vec<(Instant, String)>> {
    static LOG: OnceLock<Mutex<Vec<(Instant, String)>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .without_time()
                .with_target(false)
                .with_env_filter(tracing_subscriber::EnvFilter::new(format!(
                    "info,{}=debug",
                    carrick::agents::file_analyzer_agent::PROMPT_FINGERPRINT_TARGET
                )))
                .with_writer(Logged)
                .finish(),
        );
        Mutex::new(Vec::new())
    })
}

#[derive(Clone, Copy)]
struct Logged;

impl std::io::Write for Logged {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let line = String::from_utf8_lossy(buf).trim_end().to_string();
        log().lock().unwrap().push((Instant::now(), line));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logged {
    type Writer = Logged;

    fn make_writer(&'a self) -> Self::Writer {
        Logged
    }
}

/// What the second scan of one fixture copy came to.
struct Rescanned {
    uploaded: BTreeMap<String, String>,
    /// Requests per route the second scan sent.
    requests: BTreeMap<String, usize>,
    /// The reader's requests of the second scan: the service each was made
    /// for, and when it was sent.
    reader: Vec<(String, Instant)>,
    /// Every request of the second scan: its route, the service it was made
    /// for, and when it was counted.
    all: Vec<(String, String, Instant)>,
    lines: Vec<(Instant, String)>,
}

impl Rescanned {
    /// When the line starting `prefix` was written.
    fn at(&self, prefix: &str) -> Instant {
        self.lines
            .iter()
            .find(|(_, line)| line.contains(prefix))
            .unwrap_or_else(|| panic!("no line {prefix:?}"))
            .0
    }

    /// The seconds of the `model` phase on `service`'s line.
    fn model_secs(&self, service: &str) -> f64 {
        let line = &self
            .lines
            .iter()
            .find(|(_, line)| line.contains(&format!("Analyzed service {service} in")))
            .unwrap_or_else(|| panic!("no line for {service}"))
            .1;
        let rest = &line[line.find(" model ").expect("a model phase") + " model ".len()..];
        rest[..rest.find('s').unwrap()].parse().unwrap()
    }

    /// Each reader prompt's fingerprint line, with the service its line
    /// names, sorted: one per request sent.
    fn fingerprints(&self) -> Vec<(String, String)> {
        let mut prints: Vec<(String, String)> = self
            .lines
            .iter()
            .filter_map(|(_, line)| {
                let print = &line[line.find("analyze-file prompt ")?..];
                let named = line.find("service{name=")? + "service{name=".len();
                let service = &line[named..named + line[named..].find('}')?];
                Some((service.to_string(), print.to_string()))
            })
            .collect();
        prints.sort();
        prints
    }

    fn answers_line(&self, service: &str) -> Option<&str> {
        self.lines
            .iter()
            .find(|(_, line)| line.contains(&format!("Answers ahead for {service}:")))
            .map(|(_, line)| line.as_str())
    }

    /// When `service`'s first reader request was sent.
    fn first_ask(&self, service: &str) -> Instant {
        self.reader
            .iter()
            .filter(|(of, _)| of == service)
            .map(|(_, at)| *at)
            .min()
            .unwrap_or_else(|| panic!("{service} asked the reader nothing"))
    }
}

/// The files changed on the branch, each with its service and the service
/// declared before it. The first service, `orders-api`, serves the routes
/// and changes nothing.
const CHANGED: [(&str, &str, &str); 2] = [
    (
        "services/catalog/src/server.ts",
        "catalog-api",
        "orders-api",
    ),
    (
        "apps/storefront/src/api.ts",
        "storefront-web",
        "catalog-api",
    ),
];

/// The fixture scanned on `main`, then again once each of [`CHANGED`] has
/// changed, one service at a time, with the lane on or off.
async fn main_then_change(tmp: &Path, ahead: bool) -> Rescanned {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE);
    let repo = tmp.join("workspace");
    let _ = std::fs::remove_dir_all(&repo);
    copy_dir(&fixture, &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "main"]);
    // SAFETY: `#[serial]`, as in `offline`.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        );
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::set_var(WIDTH_ENV, "1");
        std::env::set_var("CARRICK_SIDECAR_READY_TIMEOUT_SECS", "600");
        // What a cloud runner's scan sets: the walks, the site questions and
        // the fact layer's files, so the library store is asked.
        std::env::set_var("CARRICK_POSITION_ROWS", "1");
        std::env::set_var("CARRICK_SITE_QUESTIONS", "1");
        std::env::set_var("CARRICK_FACT_FILES", "1");
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
        if ahead {
            std::env::remove_var(AHEAD_ENV);
        } else {
            std::env::set_var(AHEAD_ENV, "0");
        }
    }
    carrick::scan_health::forget_service_losses(None);
    let store = Store::default();
    let sidecar = real_sidecar(&repo);
    run_analysis_engine_with_sidecar(store.clone(), repo.to_str().unwrap(), Some(&sidecar), false)
        .await
        .expect("the scan of main succeeds");
    drop(sidecar);
    take_schedules();

    for (file, ..) in CHANGED {
        let path = repo.join(file);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("// changed on the branch\n");
        std::fs::write(&path, text).unwrap();
    }
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "change"]);

    let uploaded_before = store.uploads();
    let requests_before = agent_service::request_counts();
    log().lock().unwrap().clear();
    agent_service::set_mock_latency(ROUTE, Some(LATENCY));
    agent_service::log_requests(true);
    let sidecar = real_sidecar(&repo);
    let result = run_analysis_engine_with_sidecar(
        store.clone(),
        repo.to_str().unwrap(),
        Some(&sidecar),
        false,
    )
    .await;
    drop(sidecar);
    let request_log = agent_service::take_request_log();
    agent_service::log_requests(false);
    agent_service::set_mock_latency(ROUTE, None);
    take_schedules();
    // SAFETY: as above.
    unsafe {
        for name in [
            "CARRICK_POSITION_ROWS",
            "CARRICK_SITE_QUESTIONS",
            "CARRICK_FACT_FILES",
            AHEAD_ENV,
        ] {
            std::env::remove_var(name);
        }
    }
    result.expect("the second scan succeeds");
    let requests = agent_service::request_counts()
        .into_iter()
        .map(|(route, count)| {
            let before = requests_before.get(&route).copied().unwrap_or(0);
            (route, count - before)
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    Rescanned {
        uploaded: store.uploaded_text(uploaded_before),
        requests,
        reader: request_log
            .iter()
            .filter(|(route, ..)| route == ROUTE)
            .map(|(_, service, at)| (service.clone().unwrap_or_default(), *at - LATENCY))
            .collect(),
        all: request_log
            .into_iter()
            .map(|(route, service, at)| (route, service.unwrap_or_default(), at))
            .collect(),
        lines: std::mem::take(&mut *log().lock().unwrap()),
    }
}

/// T3: one service at a time, each changed service's files are asked while
/// the service before it is analysed, and its analysis then waits less for
/// the reader than one ask takes. The scan uploads the same blobs and sends
/// the same requests, route for route, as with nothing asked ahead, and
/// nothing asked ahead goes unused.
#[tokio::test]
#[serial]
async fn the_next_service_s_answers_are_asked_while_the_scan_analyses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let off = main_then_change(tmp.path(), false).await;
    let on = main_then_change(tmp.path(), true).await;

    assert!(
        off.requests.get(ROUTE).copied().unwrap_or(0) >= 2,
        "both changed files are asked: {:?}\n{:#?}",
        off.requests,
        off.lines
            .iter()
            .filter(|(_, line)| line.contains("Analyzed service"))
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        on.requests, off.requests,
        "the same requests, route for route"
    );
    // The run log keeps a fingerprint line per prompt sent, under the
    // service it was sent for, whether the lane or the analysis sent it.
    assert_eq!(
        on.fingerprints().len(),
        off.requests.get(ROUTE).copied().unwrap_or(0)
    );
    assert_eq!(
        on.fingerprints(),
        off.fingerprints(),
        "the same prompts on the run log, each under its service"
    );
    assert_eq!(
        on.uploaded.keys().collect::<Vec<_>>(),
        off.uploaded.keys().collect::<Vec<_>>()
    );
    for (service, blob) in &off.uploaded {
        assert!(
            on.uploaded[service] == *blob,
            "{service} uploads the same blob with the lane on"
        );
    }

    for (_, service, before) in CHANGED {
        eprintln!(
            "{service}: model phase {:.2}s with the lane, {:.2}s without; requests {:?}",
            on.model_secs(service),
            off.model_secs(service),
            on.requests
        );
        let line = on.answers_line(service).unwrap_or_default();
        assert!(
            line.ends_with(&format!(
                "Answers ahead for {service}: asked 1 file(s), taken 1, unused 0"
            )),
            "{service}: {line:?}"
        );
        assert!(off.answers_line(service).is_none());
        let ended = format!("Analyzed service {before} in");
        assert!(
            on.first_ask(service) < on.at(&ended),
            "{service}'s answer is asked while {before} is analysed"
        );
        assert!(
            off.first_ask(service) > off.at(&ended),
            "with the lane off, {service} asks in its own turn"
        );
        assert!(
            on.model_secs(service) < LATENCY.as_secs_f64(),
            "{service}'s model phase {}s waited out an ask",
            on.model_secs(service)
        );
        assert!(off.model_secs(service) >= LATENCY.as_secs_f64());
    }

    // A service none of whose files changed has nothing to ask the reader,
    // so the lane leaves it alone: it asks nothing for it, the library store
    // included, before its own analysis starts.
    let started = on.at("Analyzing service notifier");
    let early: Vec<&(String, String, Instant)> = on
        .all
        .iter()
        .filter(|(_, service, at)| service == "notifier" && *at < started)
        .collect();
    assert!(
        early.is_empty(),
        "notifier changed nothing, yet was asked for ahead: {early:?}"
    );
    assert!(on.answers_line("notifier").is_none());
}
