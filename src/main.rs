mod agent_service;
mod agents;
mod analyzer;
mod app_context;
mod boundary;
mod call_base;
mod call_graph;
mod call_site_extractor;
mod cloud_storage;
mod config;
mod credentials;
mod deno_support;
mod dispatch;
mod engine;
mod env_alias;
mod eval_output;
mod event_emitter;
mod external_call_candidates;
mod extractor;
mod file_based_router;
mod file_finder;
mod findings;
mod formatter;
mod framework_detector;
mod git_state;
mod graphql;
mod help;
mod import_bindings;
mod imported_request_member;
mod intent_generator;
mod local_http_wrapper;
mod local_mode;
mod logging;
mod mount_graph;
mod multi_agent_orchestrator;
mod new_url_target;
mod oidc;
mod operation;
mod packages;
mod parser;
mod phase_timing;
mod progress;
mod receiver_origin;
mod receiver_type;
mod scan_health;
mod sdk_edges;
mod sdk_surface;
mod service_derivation;
mod services;
mod signature_pass;
mod socket_io;
mod swc_scanner;
mod type_manifest;
mod url_normalizer;
mod utils;
mod visitor;
mod workspace_resolver;
mod wrapper_dispatch;
mod wrapper_request_shape;

use crate::cloud_storage::{AwsStorage, LocalDirStorage, MockStorage};
use crate::services::TypeSidecar;
use crate::services::type_sidecar;
use engine::run_analysis_engine_with_sidecar;
use std::env;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

/// CLI arguments for the carrick analyzer
struct CliArgs {
    /// Path to the repository to analyze
    repo_path: String,
    /// Enable verbose (debug-level) terminal output
    verbose: bool,
    /// Skip incremental cache and run a full analysis
    no_cache: bool,
}

impl CliArgs {
    fn parse() -> Self {
        let args: Vec<String> = env::args().skip(1).collect();
        Self::parse_from(&args)
    }

    fn parse_from(args: &[String]) -> Self {
        let mut repo_path = ".".to_string();
        let mut verbose = false;
        let mut no_cache = false;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--help" | "-h" => {
                    Self::print_help();
                    std::process::exit(0);
                }
                "--verbose" | "-v" => {
                    verbose = true;
                }
                "--no-cache" => {
                    no_cache = true;
                }
                arg if !arg.starts_with('-') => {
                    repo_path = arg.to_string();
                }
                _ => {
                    eprintln!("Unknown argument: {}", args[i]);
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
        }
    }

    fn print_help() {
        eprintln!("{}", crate::help::HELP);
    }
}

#[tokio::main]
async fn main() {
    // The local read-only path (carrick#708) is chosen by the first argument
    // and nothing else, so every existing invocation — `carrick .`,
    // `carrick /repo --no-cache` — reaches the scan exactly as before.
    let argv: Vec<String> = env::args().skip(1).collect();
    if let Some(parsed) = local_mode::cli::parse(&argv) {
        match parsed {
            Ok(command) => {
                // Only the writing commands log. `touch` and `check` run on
                // every edit an editor makes: a run banner, a log file and a
                // spinner are all noise in a hook's output, and the errors
                // they can raise are printed directly.
                if command.writes() {
                    logging::init(false);
                }
                std::process::exit(local_mode::cli::run(command));
            }
            Err(message) => {
                eprintln!("carrick: {message}");
                std::process::exit(2);
            }
        }
    }

    if let Some(problem) = unknown_command(&argv) {
        eprintln!("carrick: {problem}");
        std::process::exit(2);
    }

    let args = CliArgs::parse();
    logging::init(args.verbose);

    if let Err(e) = run_analysis(args).await {
        error!("Analysis failed: {}", e);
        std::process::exit(1);
    }
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
fn unknown_command(argv: &[String]) -> Option<String> {
    let word = argv
        .iter()
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
    // Validate the scan target up front. A nonexistent path would otherwise
    // walk zero files and "succeed" with an empty analysis.
    if !Path::new(&args.repo_path).is_dir() {
        return Err(format!(
            "Repository path '{}' does not exist or is not a directory",
            args.repo_path
        )
        .into());
    }

    let services = service_derivation::resolve(Path::new(&args.repo_path))?.services;
    deno_support::require_runtime(Path::new(&args.repo_path), &services)?;
    let initial_service = services.first().cloned().unwrap_or_default();

    // =======================================================================
    // STEP 1: Discover and spawn sidecar (non-blocking)
    // The sidecar is bundled with the tool - auto-discover its location
    // =======================================================================
    let sp = logging::spinner("Initializing sidecar...");
    let mut sidecar_found = false;
    let sidecar = match discover_sidecar_path() {
        Some(sidecar_path) => {
            sidecar_found = true;
            debug!("Found sidecar at: {}", sidecar_path.display());
            match spawn_sidecar(&sidecar_path, &args.repo_path, &initial_service) {
                Ok(sidecar) => {
                    debug!("Sidecar spawned, initializing in background...");
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

    if use_local_dir && cloud_storage::laptop_scan_requested() {
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

/// Discover the sidecar path by checking known locations
fn discover_sidecar_path() -> Option<PathBuf> {
    // The sidecar entry point after building (TypeScript compiles to dist/src/)
    let sidecar_entry = "dist/src/index.js";

    // List of locations to check, in order of priority
    let mut candidates: Vec<PathBuf> = vec![];

    // 0. Told where it is. The npm package (carrick#710) is the case that
    //    needs this: the binary ships in a per-platform package and the
    //    sidecar ships in the main one, beside the `node_modules/` its
    //    dependencies resolve through, so nothing relative to the executable
    //    can reach it.
    if let Ok(configured) = env::var("CARRICK_SIDECAR_DIR")
        && !configured.trim().is_empty()
    {
        candidates.push(PathBuf::from(configured));
    }

    candidates.extend([
        // 1. Relative to executable (for packaged distribution)
        get_executable_relative_path("sidecar"),
        get_executable_relative_path("../sidecar"),
        get_executable_relative_path("../lib/sidecar"),
    ]);

    // 2. For development builds, use CARGO_MANIFEST_DIR (set at compile time)
    //    This ensures we find the sidecar regardless of the current working directory
    if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
        candidates.push(PathBuf::from(manifest_dir).join("src/sidecar"));
    }

    // 3. Fallback to relative paths (in case running from project root)
    candidates.extend([
        PathBuf::from("src/sidecar"),
        PathBuf::from("./src/sidecar"),
        PathBuf::from("sidecar"),
    ]);

    for candidate in candidates {
        let full_path = candidate.join(sidecar_entry);
        if full_path.exists() {
            debug!("Checking sidecar candidate: {:?}", full_path);
            return Some(full_path);
        }
    }

    None
}

/// Get a path relative to the executable location
fn get_executable_relative_path(relative: &str) -> PathBuf {
    if let Ok(exe_path) = env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
    {
        return exe_dir.join(relative);
    }
    PathBuf::from(relative)
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
