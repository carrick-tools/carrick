//! The `carrick` binary: argument parsing, process lifecycle and exit codes
//! over the `carrick` library crate. Every scanner module lives in the
//! library (`src/lib.rs`); declaring them here again would compile the whole
//! tree a second time for the binary and its unit tests.

use carrick::cloud_storage::{self, AwsStorage, LocalDirStorage, MockStorage};
use carrick::engine::{self, run_analysis_engine_with_sidecar};
use carrick::services::TypeSidecar;
use carrick::services::type_sidecar;
use carrick::{
    analysis_channel, config, credentials, deno_support, help, keep_awake, local_mode, logging,
    panic_report, preflight, progress, scan_health, scan_stage, service_derivation, shutdown,
};
use carrick::{errln, outln};
use std::env;
use std::path::Path;
use tracing::{debug, error, info, warn};

/// CLI arguments for the carrick analyzer
struct CliArgs {
    /// Path to the repository to analyze
    repo_path: String,
    /// Enable verbose (debug-level) terminal output
    verbose: bool,
    /// Skip incremental cache and run a full analysis
    no_cache: bool,
    /// Scan a checkout that is not prepared — dependencies uninstalled, a
    /// config mapping pointing at a directory that is not there — instead of
    /// refusing it (carrick#1254).
    allow_unprepared: bool,
    /// `--repo <owner/name>`: the repository this checkout is, whatever its
    /// folder and its remote say (carrick#2207). Held as written; validated by
    /// [`declared_repo`] in `main`. Empty when the flag was the last argument.
    repo: Option<String>,
    /// `--upload` (`true`) or `--no-upload` (`false`); `None` decides from
    /// the GitHub context as before (carrick#2207).
    upload: Option<bool>,
    /// `--changed-only`: analyse only the services whose inputs changed
    /// since their stored index, and carry the rest (carrick#2227).
    changed_only: bool,
    /// `--pr`, `--pr-head`, `--pr-base`: the pull request this checkout is,
    /// outside GitHub Actions (carrick#2229). Held as written; validated by
    /// `pr_context::declare_from_flags` in `main`.
    pr: carrick::pr_context::PrFlags,
}

/// The runner's flags (carrick#2207), left out of `carrick --help`: only a
/// scan run by something other than a person passes them. Their contract is
/// `docs/reference/runner.md`.
const REPO_FLAG: &str = "--repo";
const UPLOAD_FLAG: &str = "--upload";
const NO_UPLOAD_FLAG: &str = "--no-upload";
const CHANGED_ONLY_FLAG: &str = "--changed-only";
const PR_FLAG: &str = "--pr";
const PR_HEAD_FLAG: &str = "--pr-head";
const PR_BASE_FLAG: &str = "--pr-base";
/// Flags that take the next argument as their value.
const VALUE_FLAGS: [&str; 4] = [REPO_FLAG, PR_FLAG, PR_HEAD_FLAG, PR_BASE_FLAG];

impl CliArgs {
    fn parse() -> Self {
        let args: Vec<String> = env::args().skip(1).collect();
        Self::parse_from(&args)
    }

    fn parse_from(args: &[String]) -> Self {
        let mut repo_path = ".".to_string();
        let mut verbose = false;
        let mut no_cache = false;
        let mut allow_unprepared = false;
        let mut repo = None;
        let mut upload = None;
        let mut changed_only = false;
        let mut pr = carrick::pr_context::PrFlags::default();

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--help" | "-h" => {
                    Self::print_help();
                    std::process::exit(0);
                }
                // What the npm shim asks an overridden binary before it runs
                // it, so the run says which build it was (carrick#1100).
                "--version" | "-V" => {
                    outln!("carrick {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                "--verbose" | "-v" => {
                    verbose = true;
                }
                "--no-cache" => {
                    no_cache = true;
                }
                preflight::ALLOW_FLAG => {
                    allow_unprepared = true;
                }
                // Read by `main` off the argument list, for this path and the
                // builds alike, the way `--verbose` is. Named here so the
                // parser accepts it (carrick#1889).
                keep_awake::OFF_FLAG => {}
                REPO_FLAG => {
                    i += 1;
                    repo = Some(args.get(i).cloned().unwrap_or_default());
                }
                UPLOAD_FLAG => upload = Some(true),
                NO_UPLOAD_FLAG => upload = Some(false),
                CHANGED_ONLY_FLAG => changed_only = true,
                PR_FLAG | PR_HEAD_FLAG | PR_BASE_FLAG => {
                    let flag = args[i].as_str();
                    i += 1;
                    let value = Some(args.get(i).cloned().unwrap_or_default());
                    match flag {
                        PR_FLAG => pr.number = value,
                        PR_HEAD_FLAG => pr.head = value,
                        _ => pr.base = value,
                    }
                }
                arg if !arg.starts_with('-') => {
                    repo_path = arg.to_string();
                }
                _ => {
                    errln!("Unknown argument: {}", args[i]);
                    Self::print_help();
                    std::process::exit(1);
                }
            }
            i += 1;
        }

        Self {
            repo_path,
            verbose,
            no_cache,
            allow_unprepared,
            repo,
            upload,
            changed_only,
            pr,
        }
    }

    fn print_help() {
        errln!("{}", help::HELP);
    }
}

#[tokio::main]
async fn main() {
    // The local read-only path (carrick#708) is chosen by the first argument
    // and nothing else, so every existing invocation — `carrick .`,
    // `carrick /repo --no-cache` — reaches the scan exactly as before.
    let argv: Vec<String> = env::args().skip(1).collect();
    // Whether this run may keep the machine awake is one answer for the
    // process, whichever command it is, so the flag is read here and both
    // parsers only accept it (carrick#1889).
    keep_awake::turn_off(argv.iter().any(|arg| arg == keep_awake::OFF_FLAG));
    // Internal: the install script's question in untrusted mode (carrick#2246).
    if argv.first().map(String::as_str) == Some(carrick::untrusted_yarn::COMMAND) {
        std::process::exit(carrick::untrusted_yarn::run(&argv[1..]));
    }
    if let Some(parsed) = local_mode::cli::parse(&argv) {
        match parsed {
            Ok(command) => {
                // Only the writing commands log. `touch` and `check` run on
                // every edit an editor makes: a run banner, a log file and a
                // spinner are all noise in a hook's output, and the errors
                // they can raise are printed directly.
                if !command.writes() {
                    std::process::exit(local_mode::cli::run(command));
                }
                // `--verbose` is the log level and nothing else, so it is read
                // off the argument list here rather than carried on every
                // command that accepts it. Without it the terminal shows the
                // build's own lines; with it, everything the file records —
                // the run preamble, the per-attempt retries, our debug
                // (carrick#1315).
                logging::init(verbose(&argv), logging::LogSink::Daily);
                std::process::exit(run_build(command).await);
            }
            Err(message) => {
                errln!("carrick: {message}");
                std::process::exit(2);
            }
        }
    }

    if let Some(problem) = unknown_command(&argv) {
        errln!("carrick: {problem}");
        std::process::exit(2);
    }

    let args = CliArgs::parse();
    // Before anything reads a repository name or decides to upload, so every
    // reader sees one answer for the whole run (carrick#2207). A usage error,
    // exit 2: `docs/reference/runner.md`.
    if let Err(message) = apply_runner_flags(&args) {
        errln!("carrick: {message}");
        std::process::exit(2);
    }
    // The scan uploads its log, so its lines go to a file of its own rather
    // than the day's shared one (carrick#1133).
    logging::init(args.verbose, logging::LogSink::Run);
    let repo_path = args.repo_path.clone();
    if let Some(declared) = carrick::utils::declared_repository()
        && let Some(line) = remote_disagrees(
            declared,
            carrick::git_state::remote_name(Path::new(&repo_path)).as_deref(),
        )
    {
        info!("{line}");
    }

    // A panic used to be the one death that said nothing: exit 101, a log that
    // stops mid-sentence, and a slot the cloud holds until its TTL. Installed
    // here because this is the first point at which there is a log to write
    // the panic to (carrick#1470).
    panic_report::install(repo_path.clone());

    // A signal ends the run where it stands. The analysis future is dropped
    // with everything it owns — the sidecar's `Drop` stops its process — and
    // then the cloud is told, so an interrupted laptop scan releases its slot
    // at once instead of refusing the re-run for the slot's TTL.
    //
    // The listeners are installed before the analysis starts, not when the
    // race first polls them: the scan's opening stretch (service resolution,
    // the sidecar's readiness wait) runs without yielding, and a signal that
    // lands before a handler exists takes the default action and reports
    // nothing. `biased` so a signal that is already pending wins the next poll.
    //
    // The signal arm only names the signal. Reporting and exiting happen after
    // the `select!` has finished, because the losing future is dropped only
    // then: a `process::exit` inside the arm would skip that drop, leaving the
    // sidecar process running and in-flight intent tasks still calling the
    // model while the report is sent.
    //
    // The listener outlives the race so the report can be raced against it in
    // turn: a second Ctrl-C ends this process where it stands rather than
    // being swallowed by the handler that is still installed (carrick#1235).
    //
    // The race is only half the reach, and the smaller half: both its arms are
    // polled by this one task, so a CPU-bound pass or the blocking sidecar wait
    // is a stretch in which the signal arm is never reached (carrick#1387).
    // What covers those is the watcher's own task, which records the signal
    // where the passes read it; a pass that notices returns and the run ends
    // through the failure path below.
    let mut shutdown = shutdown::watch();
    let raced = tokio::select! {
        biased;
        signal = shutdown.recv() => Err(signal),
        result = run_analysis(args) => Ok(result),
    };
    let finished = match raced {
        Ok(result) => result,
        Err(signal) => {
            error!(
                "Scan interrupted by {} during {}",
                signal.name(),
                scan_stage::current().as_str()
            );
            let ended = shutdown::within_budget(
                report_interruption(&repo_path),
                shutdown.recv(),
                shutdown::INTERRUPTION_REPORT_BUDGET,
            )
            .await;
            debug!("This run's report of its own interruption: {ended:?}");
            std::process::exit(signal.exit_code());
        }
    };

    if let Err(e) = finished {
        // A pass noticed the signal and ended the run from inside it
        // (carrick#1387). Everything the cloud is owed has already been sent on
        // the way out: an open scan is marked failed by the engine's own `Err`
        // branch, with this error's words as the reason, and the log is shipped
        // after it. What is left is to say so and to leave with the code the
        // signal names rather than with 1.
        if let Some(signal) = shutdown::requested() {
            error!(
                "Scan interrupted by {} during {}: {}",
                signal.name(),
                scan_stage::current().as_str(),
                e
            );
            progress::failed(scan_stage::current().as_str(), &e.to_string());
            // A run interrupted before `start-scan` opened a scan has had
            // nothing reported for it, and this is the one event it can send.
            // Drained first: the signal that ended the run was recorded, never
            // received, and racing the report against it unread would abandon
            // the report in its first poll.
            shutdown.drain();
            let ended = shutdown::within_budget(
                report_failure_before_scan(&repo_path, e.as_ref()),
                shutdown.recv(),
                shutdown::INTERRUPTION_REPORT_BUDGET,
            )
            .await;
            debug!("This run's report of its own interruption: {ended:?}");
            std::process::exit(signal.exit_code());
        }
        // The stage is in the line a user pastes. The same token goes to the
        // cloud as the fail marker's `stage` (carrick#1063), so a terminal and
        // a dashboard name the same thing, and a run that could reach neither
        // still says it on the way out.
        error!(
            "Analysis failed during {}: {}",
            scan_stage::current().as_str(),
            e
        );
        // The same reason, for a parent that runs this scan and reports it in
        // one sentence rather than as this process's log (carrick#1103).
        progress::failed(scan_stage::current().as_str(), &e.to_string());
        // After the line, not before it: the user reads why the run stopped
        // without waiting on a request about it.
        report_failure_before_scan(&repo_path, e.as_ref()).await;
        std::process::exit(failure_exit_code());
    }
}

/// The code a failed run exits with: [`shutdown::TYPES_OUT_OF_MEMORY_EXIT`]
/// when it stopped at a type-checker memory death it was told to stop on
/// (carrick#2208), otherwise 1. Read from the run's record rather than the
/// error, which reaches here as text.
fn failure_exit_code() -> i32 {
    match scan_health::type_memory_stop() {
        Err(_) => shutdown::TYPES_OUT_OF_MEMORY_EXIT,
        Ok(()) => 1,
    }
}

/// Record `--repo`, `--upload` / `--no-upload` and `--changed-only` for the
/// run, or the usage error one meets (carrick#2207, carrick#2227).
fn apply_runner_flags(args: &CliArgs) -> Result<(), String> {
    if let Some(value) = &args.repo {
        carrick::utils::declare_repository(declared_repo(value)?);
    }
    if let Some(upload) = args.upload {
        engine::set_upload_switch(upload);
    }
    carrick::pr_context::declare_from_flags(&args.pr, &args.repo_path)?;
    if args.changed_only {
        if args.no_cache {
            return Err(carrick::change_scope::NO_CACHE_CONFLICT.to_string());
        }
        carrick::change_scope::request();
    }
    carrick::agent_service::init_prompt_runner_from_env()?;
    engine::upload_decision_from_env()
        .map(|_| ())
        .map_err(str::to_string)
}

/// `--repo`'s value when it is an `owner/name`, else what the run is told.
fn declared_repo(value: &str) -> Result<&str, String> {
    if carrick::git_state::valid_repo_name(value) {
        Ok(value)
    } else {
        Err(format!(
            "{REPO_FLAG} takes owner/name (for example acme/api), not '{value}'."
        ))
    }
}

/// What the log says when `--repo` and the checkout's origin remote name
/// different repositories: the flag wins, and the run says so. Casing is not a
/// difference, because the cloud compares names case-insensitively.
fn remote_disagrees(declared: &str, remote: Option<&str>) -> Option<String> {
    let remote = remote?;
    if remote.eq_ignore_ascii_case(declared) {
        return None;
    }
    Some(format!(
        "This checkout's origin remote names {remote}; scanning it as {declared}, \
         from {REPO_FLAG}."
    ))
}

/// `index` and `refresh`, raced against a signal.
///
/// A build records the scan it runs, and a Ctrl-C used to end it with that
/// record still saying `running` — which `carrick status` could only call
/// "stopped without finishing" (carrick#1132). The build is synchronous, so it
/// runs on a blocking thread while this one listens; the listener is installed
/// before the build starts, for the reason the scan path gives. The scans the
/// build drives share its process group and hear the same Ctrl-C, and each
/// reports its own interruption to the cloud.
async fn run_build(command: local_mode::cli::LocalCommand) -> i32 {
    let mut shutdown = shutdown::watch();
    let mut build = tokio::task::spawn_blocking(move || local_mode::cli::run(command));
    let signal = tokio::select! {
        biased;
        signal = shutdown.recv() => signal,
        code = &mut build => return code.unwrap_or(1),
    };
    local_mode::cli::interrupted(signal.name());
    // The build is a parent: the scan it is waiting on is a process of its
    // own, and a signal sent to this pid alone never reached it. Without the
    // forward the two disagree for as long as the child runs — `carrick
    // status` says the scan was interrupted while a scan of that repo is
    // still writing an index, and a second `carrick index` is refused by a
    // slot whose run the user was told had stopped (carrick#1379).
    //
    // Waited on afterwards, because the forward is what asks the child for its
    // own bounded report and leaving at once would take that back. A child
    // that does not go inside the grace is left to the slot's own expiry,
    // exactly as it is today.
    if local_mode::cli::forward_to_running_scan(signal) {
        let _ = tokio::time::timeout(local_mode::cli::forwarded_exit_grace(), build).await;
    }
    signal.exit_code()
}

/// Tell the cloud about a laptop run that died before `start-scan` opened a
/// scan (carrick#1096), and nothing else.
///
/// A run that opened one has already sent `scan-failed` from the engine, so
/// the process-global slot `start-scan` sets is what keeps the two events
/// exclusive: one failure, one event. The storage is built only when this run
/// would have reached the cloud at all, because building it reads the
/// credential on disk — a `CARRICK_MOCK_ALL` or offline-harness run that
/// fails on a bad path must not report to production. A run whose credential
/// is itself the problem has nothing to authenticate the report with and
/// sends nothing; the OIDC path sends nothing by the storage's own gate.
async fn report_failure_before_scan(repo_path: &str, error: &dyn std::error::Error) {
    let reaches_cloud = run_reaches_cloud(
        env::var(cloud_storage::CACHE_DIR_ENV).is_ok(),
        cloud_storage::laptop_scan_requested(),
        env::var("CARRICK_MOCK_ALL").is_ok(),
    );
    if !should_report_preflight(credentials::scan_id().is_some(), reaches_cloud) {
        return;
    }
    match AwsStorage::new(false) {
        Ok(storage) => {
            engine::report_preflight_failure(&storage, repo_path, scan_stage::current(), error)
                .await
        }
        Err(e) => debug!("No pre-scan failure report: {e}"),
    }
}

/// Which event an interrupted run sends.
#[derive(Debug, PartialEq, Eq)]
enum InterruptionReport {
    /// A run that never reaches the cloud tells nobody.
    Nothing,
    /// `start-scan` opened a scan: mark it, which releases its slot.
    ScanFailed,
    /// The run stopped before a scan was opened (carrick#1096).
    PreflightFailed,
}

fn interruption_report(scan_opened: bool, reaches_cloud: bool) -> InterruptionReport {
    match (reaches_cloud, scan_opened) {
        (false, _) => InterruptionReport::Nothing,
        (true, true) => InterruptionReport::ScanFailed,
        (true, false) => InterruptionReport::PreflightFailed,
    }
}

/// Tell the cloud this run was interrupted, best effort, in the stage it was
/// in: `scan-failed` when a scan was open, `preflight-failed` before that.
async fn report_interruption(repo_path: &str) {
    let reaches_cloud = run_reaches_cloud(
        env::var(cloud_storage::CACHE_DIR_ENV).is_ok(),
        cloud_storage::laptop_scan_requested(),
        env::var("CARRICK_MOCK_ALL").is_ok(),
    );
    let scan_id = credentials::scan_id();
    let report = interruption_report(scan_id.is_some(), reaches_cloud);
    if report == InterruptionReport::Nothing {
        return;
    }
    let storage = match AwsStorage::new(false) {
        Ok(storage) => storage,
        Err(e) => {
            debug!("No interruption report: {e}");
            return;
        }
    };
    // The slot this run holds, which the signal dropped the engine and its
    // storage with. Both the marker and the log have to name it: a laptop
    // log is stored against its scan, and one that names none can only be
    // answered `404 scan_not_started` (carrick#1370).
    if let Some(scan_id) = scan_id {
        storage.adopt_scan_id(scan_id);
    }
    report_interruption_to(&storage, repo_path, report, logging::run_log_file()).await;
}

/// The report itself: the marker, and then this run's log.
///
/// Separated from the gates above so it can be driven against a storage that
/// records what it was handed. The order is the engine's, and for the same
/// reason ([`engine::run_analysis_engine_with_sidecar`]): the marker is four
/// short fields and the log is up to five megabytes, and a process that is
/// already going may not get to finish both. Nothing here waits on either;
/// the caller races the whole of it against a second signal and the budget.
///
/// The log was simply never sent on this path (carrick#1473). The only call
/// to the upload was on the analysis future's return, and a dropped future
/// never reaches it, so the one death class the cloud did hear about arrived
/// as four fields and nothing to read.
async fn report_interruption_to<T: cloud_storage::CloudStorage + Sync>(
    storage: &T,
    repo_path: &str,
    report: InterruptionReport,
    log: Option<&Path>,
) {
    match report {
        InterruptionReport::Nothing => return,
        InterruptionReport::ScanFailed => {
            storage
                .report_scan_failed(scan_stage::current().as_str(), shutdown::INTERRUPTED_REASON)
                .await
        }
        InterruptionReport::PreflightFailed => {
            let error = std::io::Error::other(shutdown::INTERRUPTED_REASON);
            engine::report_preflight_failure(storage, repo_path, scan_stage::current(), &error)
                .await
        }
    }
    engine::upload_run_log_from(storage, repo_path, log).await;
}

/// Whether a run with these settings talks to Carrick Cloud: the same
/// precedence the storage selection in [`run_analysis`] applies, where
/// `CARRICK_MOCK_ALL` beats everything and an offline cache directory is the
/// cloud only when a laptop scan was asked for.
fn run_reaches_cloud(use_local_dir: bool, laptop_scan: bool, use_mock: bool) -> bool {
    !use_mock && (!use_local_dir || laptop_scan)
}

/// A pre-scan failure event is sent only by a run that reaches the cloud and
/// never opened a scan; an opened scan reports through `scan-failed`.
fn should_report_preflight(scan_opened: bool, reaches_cloud: bool) -> bool {
    reaches_cloud && !scan_opened
}

/// A first argument that is neither a command nor a path, and what to say
/// about it.
///
/// `carrick whoami` used to reach the scan and answer "Repository path
/// 'whoami' does not exist or is not a directory", which reads as a broken
/// checkout rather than a mistyped command and names no alternative
/// (carrick#997 item 6). What counts as a path is anything that exists on
/// disk, and anything WRITTEN as one — a separator, a leading dot. A bare word
/// that is neither is a command, and there are exactly two kinds: the ones the
/// npm package serves, which are named rather than denied, and the rest.
/// Whether this run was asked for the verbose log level.
///
/// The scan path reads it through [`CliArgs`]; the local commands have their
/// own parser, which treats the flag as the global it is and leaves the answer
/// to this (carrick#1315).
fn verbose(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "--verbose" || arg == "-v")
}

fn unknown_command(argv: &[String]) -> Option<String> {
    // `--repo`'s value is the flag's, never a command: `carrick --repo api .`
    // is answered by the flag's own usage error, not as a mistyped `api`.
    let word = argv
        .iter()
        .enumerate()
        .filter(|(index, _)| *index == 0 || !VALUE_FLAGS.contains(&argv[index - 1].as_str()))
        .map(|(_, arg)| arg)
        .find(|arg| !arg.starts_with('-'))
        .filter(|arg| !arg.is_empty())?;
    let written_as_a_path = word.starts_with('.')
        || word.contains('/')
        || word.contains(std::path::MAIN_SEPARATOR)
        || Path::new(word).exists();
    if written_as_a_path {
        return None;
    }
    if help::PACKAGE_COMMANDS.contains(&word.as_str()) {
        return Some(format!(
            "`carrick {word}` comes from the carrick npm package, and this is the scanner \
             binary on its own. Install the package with `npm i -g carrick` and run it from \
             there."
        ));
    }
    Some(format!(
        "`{word}` is not a carrick command. Commands: {}, and {} from the npm package. To \
         scan a repository, pass a path that exists (`carrick .`). `carrick --help` lists \
         them all.",
        local_mode::cli::LOCAL_COMMANDS.join(", "),
        help::PACKAGE_COMMANDS.join(", ")
    ))
}

async fn run_analysis(args: CliArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Everything before the engine is preflight: the path, the services, the
    // runtime they need, the sidecar, and the storage. Said here rather than
    // left at `unknown`, so a run that stops at a missing runtime names the
    // stage it stopped in to the terminal and to the cloud (carrick#1096).
    scan_stage::enter(scan_stage::Stage::Preflight);

    // Validate the scan target up front. A nonexistent path would otherwise
    // walk zero files and "succeed" with an empty analysis.
    if !Path::new(&args.repo_path).is_dir() {
        return Err(format!(
            "Repository path '{}' does not exist or is not a directory",
            args.repo_path
        )
        .into());
    }

    let services = service_derivation::resolve_for_scan(Path::new(&args.repo_path))?;
    deno_support::require_runtime(Path::new(&args.repo_path), &services)?;
    // Before the sidecar, before the first model call, and before anything is
    // charged for: a tree that is not prepared indexes `any` where a package
    // or a generated module should be, and the user is told nothing
    // (carrick#1254).
    preflight::allow(args.allow_unprepared);
    // Only a run that is going to ask the model about this tree: the join and
    // the re-check pass over blobs that a scan already wrote, and a `resume`
    // collects answers the model has already given for it — refusing either
    // would strand work rather than save any.
    if !local_mode::no_model() && env::var_os(analysis_channel::ANSWERS_ENV).is_none() {
        preflight::require_prepared(Path::new(&args.repo_path), &services)?;
    }
    // From here the run is going to happen, and its first wait is the
    // sidecar's: hold the machine awake until this function is left
    // (carrick#1889). After every refusal above, so a run that is refused says
    // nothing about a hold it never took. A scan a build started holds
    // nothing: the build holds for it. See `keep_awake` for why the hold
    // cannot outlive this process on the paths that never reach the drop.
    let _awake = keep_awake::begin(|line| info!("{line}"));
    let initial_service = services.first().cloned().unwrap_or_default();

    // =======================================================================
    // STEP 1: Discover and spawn sidecar (non-blocking)
    // The sidecar is bundled with the tool - auto-discover its location
    // =======================================================================
    let sp = logging::spinner("Initializing sidecar...");
    let mut sidecar_found = false;
    let sidecar = match type_sidecar::discover_sidecar_path() {
        Some(sidecar_path) => {
            sidecar_found = true;
            debug!("Found sidecar at: {}", sidecar_path.display());
            match spawn_sidecar(&sidecar_path, &args.repo_path, &initial_service) {
                Ok(sidecar) => {
                    debug!("Sidecar spawned, initializing in background...");
                    // A local join asks the resident type daemon its `check`
                    // handed it for the type check first (carrick#2883); any
                    // failure there is checked in this process instead.
                    if env::var_os(carrick::local_mode::JOIN_OUT_ENV).is_some() {
                        sidecar.set_check_daemon(carrick::services::typed_daemon::Use::from_env());
                    }
                    Some(sidecar)
                }
                Err(e) => {
                    warn!("Failed to spawn sidecar: {}", e);
                    None
                }
            }
        }
        None => {
            debug!("Sidecar not found, continuing without type extraction");
            None
        }
    };

    // =======================================================================
    // STEP 2: Wait for sidecar to be ready (if spawned) before analysis
    // The sidecar initializes in parallel, so it should be ready by now
    // =======================================================================
    let budget = type_sidecar::ready_budget();
    let sidecar_ready = if let Some(ref sidecar) = sidecar {
        debug!("Waiting for sidecar to be ready...");
        match sidecar.wait_ready(budget) {
            Ok(()) => {
                logging::finish_spinner(&sp, "Sidecar ready");
                true
            }
            Err(e) => {
                // A wait this process was signalled out of says nothing about
                // the sidecar, and the run stops for the reason it really
                // stopped for rather than for a readiness budget it never
                // served (carrick#1387).
                shutdown::check()?;
                logging::finish_spinner_warn(&sp, "Sidecar unavailable");
                // Without the sidecar every endpoint this run indexes carries
                // no request or response type. The scan can still produce a
                // route surface, so nothing downstream fails and the run
                // reports success — which is how a rescan replaced a typed
                // index with a typeless one and only an external gate's type
                // rows noticed (carrick#748). Stop here instead: before the
                // analysis, so the LLM spend is not paid for a typeless
                // result, and before the upload, so the typed index already
                // in the cloud survives.
                if scan_health::should_fail_on_missing_types(
                    e.is_environmental(),
                    scan_health::allow_missing_types_from_env(),
                ) {
                    let message = format!(
                        "The type sidecar was not ready within {}s ({}), so this scan \
                         would index every endpoint with no request or response types. \
                         Aborting before the analysis so the existing index keeps its \
                         types. Raise the budget with {}, or set {}=1 to scan without \
                         types anyway.",
                        budget.as_secs(),
                        e,
                        type_sidecar::READY_TIMEOUT_ENV,
                        scan_health::ALLOW_MISSING_TYPES_ENV,
                    );
                    logging::annotate(logging::Annotation::Error, &message);
                    return Err(message.into());
                }
                warn!("Sidecar failed to initialize: {}", e);
                scan_health::record_types_unavailable(scan_health::WHOLE_SCAN, &e.to_string());
                false
            }
        }
    } else if sidecar_found {
        logging::finish_spinner_warn(&sp, "Sidecar failed to start");
        scan_health::record_types_unavailable(scan_health::WHOLE_SCAN, "sidecar failed to start");
        false
    } else {
        logging::finish_spinner_warn(&sp, "Sidecar not found");
        scan_health::record_types_unavailable(scan_health::WHOLE_SCAN, "sidecar not found");
        false
    };

    // =======================================================================
    // STEP 3: Run analysis engine with sidecar (if ready)
    // =======================================================================

    // Storage selection precedence:
    //   1. CARRICK_LOCAL_STORAGE_DIR set -> LocalDirStorage (offline eval harness).
    //      Its only job is ISOLATION: upload writes CloudRepoData to a cache dir
    //      and download reads it back (or returns empty under
    //      CARRICK_LOCAL_STORAGE_ISOLATE=1), so the cross-repo join never reaches
    //      the real cloud and a per-repo Phase-A scan can't pick up siblings.
    //   2. CARRICK_MOCK_ALL set -> MockStorage (in-memory, synthetic siblings).
    //   3. otherwise -> AwsStorage (production).
    // The engine stays storage-agnostic — same pattern as MockStorage.
    let use_local_dir = env::var(cloud_storage::CACHE_DIR_ENV).is_ok();
    // Use MockStorage if CARRICK_MOCK_ALL env var is set, otherwise use AWS Storage
    let use_mock = env::var("CARRICK_MOCK_ALL").is_ok();

    // Pass sidecar reference if it's ready
    let sidecar_ref = if sidecar_ready {
        sidecar.as_ref()
    } else {
        None
    };

    // `CARRICK_MOCK_ALL` beats the laptop branch: the variable means "use mock
    // storage instead of Carrick Cloud" (`--help`), and a laptop scan is the
    // one path that would have reached the cloud with it set. Without this,
    // no test could drive `carrick index` — which since carrick#1008 always
    // infers — without real credentials and real spend.
    if use_local_dir && cloud_storage::laptop_scan_requested() && !use_mock {
        // A laptop scan: the model answers through the cloud, the index is
        // uploaded, and the same payload is written to the cache directory so
        // the local read model is built from the run that produced it rather
        // than from a later download (carrick#956 §8.3).
        info!("Using TeeStorage (laptop scan: cloud upload + local cache)");
        let storage = cloud_storage::TeeStorage::from_env(args.no_cache)?;
        run_analysis_engine_with_sidecar(storage, &args.repo_path, sidecar_ref, args.no_cache).await
    } else if use_local_dir {
        info!("Using LocalDirStorage (offline eval harness)");
        let storage = LocalDirStorage::from_env()?;
        run_analysis_engine_with_sidecar(storage, &args.repo_path, sidecar_ref, args.no_cache).await
    } else if use_mock {
        info!("Using MockStorage");
        let storage = MockStorage::new();
        run_analysis_engine_with_sidecar(storage, &args.repo_path, sidecar_ref, args.no_cache).await
    } else {
        // `--no-cache` is carried into the upload as well as the analysis: a
        // run that re-analyzed every file supersedes the stored generation
        // even when the commit has not moved (carrick#885).
        let storage = AwsStorage::new(args.no_cache)?;
        run_analysis_engine_with_sidecar(storage, &args.repo_path, sidecar_ref, args.no_cache).await
    }

    // Sidecar will be automatically shut down when it goes out of scope (Drop impl)
}

/// Spawn the type sidecar and start initialization
fn spawn_sidecar(
    sidecar_path: &Path,
    repo_path: &str,
    service: &config::Config,
) -> Result<TypeSidecar, Box<dyn std::error::Error>> {
    // Convert repo path to absolute path for the sidecar
    // The sidecar runs as a separate process and needs an absolute path
    let repo_path = Path::new(repo_path);
    let absolute_repo_path = if repo_path.is_absolute() {
        repo_path.to_path_buf()
    } else {
        let cwd = env::current_dir()
            .map_err(|e| format!("Failed to get current working directory: {}", e))?;
        debug!("Current working directory: {:?}", cwd);
        debug!("Repo path (relative): {:?}", repo_path);
        cwd.join(repo_path)
    };

    debug!("Repo path (before canonicalize): {:?}", absolute_repo_path);

    // Canonicalize to resolve any .. or . segments in the path
    let absolute_repo_path = absolute_repo_path.canonicalize().map_err(|e| {
        format!(
            "Failed to canonicalize repo path '{}': {}. \
            Make sure the path exists and you're running from the correct directory.",
            absolute_repo_path.display(),
            e
        )
    })?;

    debug!("Repo path (canonicalized): {:?}", absolute_repo_path);

    // Spawn the sidecar process
    let sidecar = TypeSidecar::spawn(sidecar_path)?;
    sidecar.set_scan_root(&absolute_repo_path);

    sidecar.start_init(
        &absolute_repo_path.join(service.directory.as_deref().unwrap_or(".")),
        service.tsconfig.as_deref(),
    );

    Ok(sidecar)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(input: &[&str]) -> Vec<String> {
        input.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_defaults() {
        let cli = CliArgs::parse_from(&args(&[]));
        assert_eq!(cli.repo_path, ".");
        assert!(!cli.verbose);
        assert!(!cli.no_cache);
    }

    #[test]
    fn test_repo_path() {
        let cli = CliArgs::parse_from(&args(&["/some/path"]));
        assert_eq!(cli.repo_path, "/some/path");
    }

    #[test]
    fn test_verbose_short() {
        let cli = CliArgs::parse_from(&args(&["-v"]));
        assert!(cli.verbose);
    }

    #[test]
    fn test_verbose_long() {
        let cli = CliArgs::parse_from(&args(&["--verbose"]));
        assert!(cli.verbose);
    }

    #[test]
    fn test_no_cache() {
        let cli = CliArgs::parse_from(&args(&["--no-cache"]));
        assert!(cli.no_cache);
        assert!(!cli.verbose);
    }

    #[test]
    fn test_no_cache_with_repo_path() {
        let cli = CliArgs::parse_from(&args(&["--no-cache", "/my/repo"]));
        assert!(cli.no_cache);
        assert_eq!(cli.repo_path, "/my/repo");
    }

    #[test]
    fn test_all_flags() {
        let cli = CliArgs::parse_from(&args(&["-v", "--no-cache", "/my/repo"]));
        assert!(cli.verbose);
        assert!(cli.no_cache);
        assert_eq!(cli.repo_path, "/my/repo");
    }

    /// The scan path accepts the flag that lets the machine sleep
    /// (carrick#1889). An argument this parser does not know ends the process,
    /// so returning at all is the assertion, and the path beside the flag is
    /// still read as the path.
    #[test]
    fn the_scan_accepts_the_flag_that_lets_the_machine_sleep() {
        let cli = CliArgs::parse_from(&args(&[keep_awake::OFF_FLAG, "/my/repo"]));
        assert_eq!(cli.repo_path, "/my/repo");
    }

    /// The runner's flags (carrick#2207): `--repo` takes the next argument as
    /// its value, never as the path, and neither flag is set by default.
    #[test]
    fn the_scan_reads_the_runner_flags() {
        let cli = CliArgs::parse_from(&args(&["--repo", "acme/api", "/work/checkout", "--upload"]));
        assert_eq!(cli.repo.as_deref(), Some("acme/api"));
        assert_eq!(cli.repo_path, "/work/checkout");
        assert_eq!(cli.upload, Some(true));

        let cli = CliArgs::parse_from(&args(&["/work/checkout", "--no-upload"]));
        assert_eq!(cli.upload, Some(false));
        assert_eq!(cli.repo, None);

        let cli = CliArgs::parse_from(&args(&["/work/checkout", "--repo"]));
        assert_eq!(cli.repo.as_deref(), Some(""));

        let cli = CliArgs::parse_from(&args(&["/work/checkout"]));
        assert_eq!((cli.repo, cli.upload), (None, None));
        assert!(!cli.changed_only);

        let cli = CliArgs::parse_from(&args(&["/work/checkout", "--changed-only"]));
        assert!(cli.changed_only);
        assert_eq!(cli.repo_path, "/work/checkout");
    }

    /// `--changed-only` beside `--no-cache` is a usage error: one asks for
    /// every service to be analysed again (carrick#2227).
    #[test]
    fn changed_only_refuses_no_cache() {
        let cli = CliArgs::parse_from(&args(&["--changed-only", "--no-cache", "."]));
        assert_eq!(
            apply_runner_flags(&cli),
            Err(
                "--changed-only cannot be combined with --no-cache, which re-analyses every \
                 service."
                    .to_string()
            )
        );
    }

    /// `--repo` takes an `owner/name` and nothing else, and says so with the
    /// value it was given.
    #[test]
    fn a_repo_flag_that_is_not_owner_and_name_is_a_usage_error() {
        assert_eq!(declared_repo("acme/api"), Ok("acme/api"));
        for value in ["api", "acme/api/extra", "../api", "acme/", ""] {
            assert_eq!(
                declared_repo(value),
                Err(format!(
                    "--repo takes owner/name (for example acme/api), not '{value}'."
                )),
                "{value}"
            );
        }
    }

    /// The run says when the flag overrides the origin remote, and only then.
    #[test]
    fn a_remote_naming_another_repository_is_said_once() {
        assert_eq!(remote_disagrees("acme/api", Some("acme/api")), None);
        assert_eq!(remote_disagrees("acme/api", Some("Acme/API")), None);
        assert_eq!(remote_disagrees("acme/api", None), None);
        let line = remote_disagrees("acme/api", Some("acme/api-fork")).expect("a line");
        assert!(
            line.contains("acme/api-fork") && line.contains("acme/api"),
            "{line}"
        );
    }

    /// The PR flags take the next argument as their value, and it is never
    /// read as a mistyped command (carrick#2229).
    #[test]
    fn pr_flag_values_are_the_flags_own() {
        let sha = "a".repeat(40);
        let cli = CliArgs::parse_from(&args(&[
            "/work/checkout",
            "--pr",
            "7",
            "--pr-head",
            &sha,
            "--pr-base",
            &sha,
        ]));
        assert_eq!(cli.pr.number.as_deref(), Some("7"));
        assert_eq!(cli.pr.head.as_deref(), Some(sha.as_str()));
        assert_eq!(cli.pr.base.as_deref(), Some(sha.as_str()));
        assert_eq!(
            CliArgs::parse_from(&args(&["/work/checkout"])).pr,
            Default::default()
        );
        assert_eq!(unknown_command(&args(&["--pr", "7", "."])), None);
        assert_eq!(unknown_command(&args(&["--pr-head", "abc", "."])), None);
        assert_eq!(unknown_command(&args(&["--pr-base", "abc", "."])), None);
    }

    /// `--repo`'s value is never read as a mistyped command, wherever the
    /// flag sits; a bare word elsewhere still is.
    #[test]
    fn a_repo_flag_value_is_not_a_command() {
        assert_eq!(unknown_command(&args(&["--repo", "api", "."])), None);
        assert_eq!(unknown_command(&args(&["--repo", "acme/api"])), None);
        assert!(unknown_command(&args(&["--repo", "acme/api", "whoami"])).is_some());
    }

    /// Only a run that talks to the cloud reports a pre-scan failure
    /// (carrick#1096): `CARRICK_MOCK_ALL` and the offline harness never do, so
    /// a test that expects a bad path to fail cannot post to production.
    #[test]
    fn only_a_run_that_reaches_the_cloud_reports_a_pre_scan_failure() {
        // (local cache dir, laptop scan, mock) -> reaches the cloud
        assert!(run_reaches_cloud(false, false, false), "plain `carrick .`");
        assert!(
            run_reaches_cloud(true, true, false),
            "`carrick index` child"
        );
        assert!(!run_reaches_cloud(true, false, false), "offline harness");
        assert!(!run_reaches_cloud(false, false, true), "mock");
        assert!(
            !run_reaches_cloud(true, true, true),
            "mock beats the laptop scan"
        );
    }

    /// A run whose `start-scan` succeeded posts no pre-scan event: the engine
    /// already sent `scan-failed`, and one failure is one event.
    #[test]
    fn a_run_that_opened_a_scan_posts_no_pre_scan_event() {
        assert!(should_report_preflight(false, true));
        assert!(!should_report_preflight(true, true));
        assert!(!should_report_preflight(false, false));
        assert!(!should_report_preflight(true, false));
    }

    /// An interrupted run sends one event, and which one depends on whether a
    /// scan was open: `scan-failed` releases the slot `start-scan` claimed, and
    /// a run stopped before that has only `preflight-failed` to say it with.
    #[test]
    fn an_interrupted_run_reports_on_the_event_its_stage_allows() {
        assert_eq!(
            interruption_report(true, true),
            InterruptionReport::ScanFailed
        );
        assert_eq!(
            interruption_report(false, true),
            InterruptionReport::PreflightFailed
        );
        assert_eq!(
            interruption_report(true, false),
            InterruptionReport::Nothing
        );
        assert_eq!(shutdown::Shutdown::Interrupt.exit_code(), 130);
        assert_eq!(shutdown::Shutdown::Terminate.exit_code(), 143);
    }

    /// A mistyped command is answered as a command, and the answer names the
    /// commands there are (carrick#997 item 6).
    #[test]
    fn a_bare_word_that_is_not_a_command_is_not_read_as_a_repository() {
        let problem = unknown_command(&args(&["whoami"])).expect("an unknown command");
        assert!(problem.contains("not a carrick command"), "{problem}");
        assert!(problem.contains("status"), "{problem}");
        assert!(problem.contains("pass a path that exists"), "{problem}");
    }

    /// The package's own commands are named, not denied: the binary cannot run
    /// `carrick init`, and "unknown command" would be a lie about where the
    /// first run starts (carrick#997 item 5).
    #[test]
    fn a_package_command_says_where_it_lives() {
        let problem = unknown_command(&args(&["init"])).expect("a package command");
        assert!(problem.contains("npm i -g carrick"), "{problem}");
    }

    /// An interrupted run sends its marker and then its log, and the log is
    /// this run's own file (carrick#1473).
    ///
    /// Before this, the only call to the upload was on the analysis future's
    /// return path, which a dropped future never reaches: the one death the
    /// cloud did hear about arrived as four fields with nothing to read
    /// beside them.
    #[tokio::test]
    async fn an_interrupted_run_ships_its_log_after_its_marker() {
        let log = tempfile::NamedTempFile::new().expect("a log for the run");
        std::fs::write(log.path(), "a line this run wrote\n").expect("write the log");
        let storage = MockStorage::new();

        report_interruption_to(
            &storage,
            ".",
            InterruptionReport::ScanFailed,
            Some(log.path()),
        )
        .await;

        let marked = storage.scan_failures();
        assert_eq!(marked.len(), 1, "{marked:?}");
        assert_eq!(marked[0].1, shutdown::INTERRUPTED_REASON);
        let shipped = storage.uploaded_logs();
        assert_eq!(shipped.len(), 1, "the log went with the marker");
        assert!(
            shipped[0].contains("a line this run wrote"),
            "{}",
            shipped[0]
        );
    }

    /// A run stopped before `start-scan` answered says so through the other
    /// event, and the log is offered on that path too.
    ///
    /// Offered, not sent: whether it leaves is the storage's own gate, and a
    /// Bearer credential with no scan to store a log against answers no
    /// (carrick#1370). The mock has no such gate, so what this pins is that
    /// the path asks.
    #[tokio::test]
    async fn a_run_interrupted_before_its_scan_opened_reports_the_other_event() {
        let log = tempfile::NamedTempFile::new().expect("a log for the run");
        std::fs::write(log.path(), "a line this run wrote\n").expect("write the log");
        let storage = MockStorage::new();

        report_interruption_to(
            &storage,
            ".",
            InterruptionReport::PreflightFailed,
            Some(log.path()),
        )
        .await;

        assert!(storage.scan_failures().is_empty());
        let before = storage.preflight_failures();
        assert_eq!(before.len(), 1, "{before:?}");
        assert_eq!(before[0].2, shutdown::INTERRUPTED_REASON);
        assert_eq!(storage.uploaded_logs().len(), 1);
    }

    /// Anything written as a path still reaches the scan, and a path that does
    /// not exist still gets the scan's own error about the path.
    #[test]
    fn a_path_is_left_to_the_scan() {
        assert_eq!(unknown_command(&args(&["."])), None);
        assert_eq!(unknown_command(&args(&["../sibling"])), None);
        assert_eq!(unknown_command(&args(&["/repos/api", "--no-cache"])), None);
        assert_eq!(unknown_command(&args(&["--verbose"])), None);
        assert_eq!(unknown_command(&args(&[])), None);
        // A bare word that is a directory on disk: cargo runs a unit test with
        // the package root as its working directory, so `src` is one.
        assert_eq!(
            unknown_command(&args(&["src"])),
            None,
            "a bare name that exists is a repository path"
        );
    }
}
