mod agent_service;
mod agents;
mod analyzer;
mod app_context;
mod call_graph;
mod call_site_extractor;
mod cloud_storage;
mod config;
mod engine;
mod env_alias;
mod eval_output;
mod external_call_candidates;
mod extractor;
mod file_based_router;
mod file_finder;
mod findings;
mod formatter;
mod framework_detector;
mod graphql;
mod import_bindings;
mod imported_request_member;
mod intent_generator;
mod local_http_wrapper;
mod logging;
mod mount_graph;
mod multi_agent_orchestrator;
mod new_url_target;
mod oidc;
mod operation;
mod packages;
mod parser;
mod scan_health;
mod sdk_edges;
mod sdk_surface;
mod services;
mod signature_pass;
mod socket_io;
mod swc_scanner;
mod type_manifest;
mod url_normalizer;
mod utils;
mod visitor;
mod workspace_resolver;
mod wrapper_request_shape;

use crate::cloud_storage::{AwsStorage, LocalDirStorage, MockStorage};
use crate::services::TypeSidecar;
use engine::run_analysis_engine_with_sidecar;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;
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
        eprintln!(
            r#"Carrick - API Contract Analyzer

USAGE:
    carrick [OPTIONS] [REPO_PATH]

ARGUMENTS:
    [REPO_PATH]    Path to the repository to analyze (default: current directory)

OPTIONS:
    -h, --help     Print this help message
    -v, --verbose  Enable verbose (debug-level) terminal output
    --no-cache     Skip incremental cache and run a full analysis

ENVIRONMENT VARIABLES:
    ACTIONS_ID_TOKEN_REQUEST_URL    GitHub Actions OIDC token endpoint (auto-set
                                    when the job grants `id-token: write`)
    ACTIONS_ID_TOKEN_REQUEST_TOKEN  Bearer token for the OIDC endpoint (auto-set)
    CARRICK_MOCK_ALL                Use mock storage instead of Carrick Cloud
    CARRICK_API_ENDPOINT            API endpoint for the carrick service (build-time)
    CARRICK_INTENT_CONCURRENCY      Concurrent function-intent requests (default 8).
                                    Lower it if a large repo loses intents to
                                    backend overload; capped by
                                    CARRICK_CONCURRENCY_LIMIT
    CARRICK_ALLOW_PARTIAL_ANALYSIS  Upload and exit 0 even when files were not
                                    analysed. Off by default: a run that lost
                                    analyzer results is reported and fails,
                                    rather than overwriting the index with a
                                    thinner one
"#
        );
    }
}

#[tokio::main]
async fn main() {
    let args = CliArgs::parse();
    logging::init(args.verbose);

    if let Err(e) = run_analysis(args).await {
        error!("Analysis failed: {}", e);
        std::process::exit(1);
    }
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

    // A Deno-native project (deno.json/deno.jsonc, no package.json anywhere)
    // would otherwise produce a silently thin scan: dependency discovery,
    // framework detection, and type resolution all start from a package.json
    // manifest.
    if is_deno_native_project(Path::new(&args.repo_path)) {
        return Err(format!(
            "'{}' looks like a Deno-native project (deno.json/deno.jsonc present, no package.json \
             anywhere in the tree). Carrick can't scan Deno-native projects yet — \
             dependency discovery, framework detection, and type resolution all start \
             from a package.json. A project that also maintains a package.json \
             (npm-compatibility mode) may scan partially. If Deno support matters to \
             you, please open an issue: https://github.com/carrick-tools/carrick/issues",
            args.repo_path
        )
        .into());
    }

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
            match spawn_sidecar(&sidecar_path, &args.repo_path) {
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
    let sidecar_ready = if let Some(ref sidecar) = sidecar {
        debug!("Waiting for sidecar to be ready...");
        match sidecar.wait_ready(Duration::from_secs(30)) {
            Ok(()) => {
                logging::finish_spinner(&sp, "Sidecar ready");
                true
            }
            Err(e) => {
                warn!("Sidecar failed to initialize: {}", e);
                logging::finish_spinner_warn(&sp, "Sidecar unavailable");
                false
            }
        }
    } else if sidecar_found {
        logging::finish_spinner_warn(&sp, "Sidecar failed to start");
        false
    } else {
        logging::finish_spinner_warn(&sp, "Sidecar not found");
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

    if use_local_dir {
        info!("Using LocalDirStorage (offline eval harness)");
        let storage = LocalDirStorage::from_env()?;
        run_analysis_engine_with_sidecar(storage, &args.repo_path, sidecar_ref, args.no_cache).await
    } else if use_mock {
        info!("Using MockStorage");
        let storage = MockStorage::new();
        run_analysis_engine_with_sidecar(storage, &args.repo_path, sidecar_ref, args.no_cache).await
    } else {
        let storage = AwsStorage::new()?;
        run_analysis_engine_with_sidecar(storage, &args.repo_path, sidecar_ref, args.no_cache).await
    }

    // Sidecar will be automatically shut down when it goes out of scope (Drop impl)
}

/// Discover the sidecar path by checking known locations
fn discover_sidecar_path() -> Option<PathBuf> {
    // The sidecar entry point after building (TypeScript compiles to dist/src/)
    let sidecar_entry = "dist/src/index.js";

    // List of locations to check, in order of priority
    let mut candidates: Vec<PathBuf> = vec![
        // 1. Relative to executable (for packaged distribution)
        get_executable_relative_path("sidecar"),
        get_executable_relative_path("../sidecar"),
        get_executable_relative_path("../lib/sidecar"),
    ];

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

/// True when the scan target is a Deno-native project: a `deno.json` /
/// `deno.jsonc` at the root and no `package.json` anywhere in the tree
/// (dependency/build directories excluded). A repo that keeps both manifests
/// (Deno's npm-compatibility mode) is not flagged — it may scan partially.
fn is_deno_native_project(repo_root: &Path) -> bool {
    let has_deno_config =
        repo_root.join("deno.json").is_file() || repo_root.join("deno.jsonc").is_file();
    if !has_deno_config {
        return false;
    }

    // depth() == 0 keeps the scan root traversable even when its own basename
    // matches a skip dir (a service directory named `build` is still a repo).
    let walker = walkdir::WalkDir::new(repo_root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !(e.file_type().is_dir()
                    && e.file_name()
                        .to_str()
                        .is_some_and(|n| packages::MANIFEST_SKIP_DIRS.contains(&n)))
        });
    !walker
        .flatten()
        .any(|entry| entry.file_type().is_file() && entry.file_name() == "package.json")
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

    sidecar.start_init(&absolute_repo_path, None);

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

    #[test]
    fn deno_native_project_is_flagged() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("deno.json"), "{}").unwrap();
        assert!(is_deno_native_project(repo.path()));
    }

    #[test]
    fn deno_jsonc_counts_as_deno_config() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("deno.jsonc"), "{}").unwrap();
        assert!(is_deno_native_project(repo.path()));
    }

    #[test]
    fn deno_with_package_json_is_not_flagged() {
        // npm-compatibility mode: both manifests present → scan proceeds.
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("deno.json"), "{}").unwrap();
        std::fs::write(repo.path().join("package.json"), "{}").unwrap();
        assert!(!is_deno_native_project(repo.path()));
    }

    #[test]
    fn deno_with_nested_package_json_is_not_flagged() {
        // A workspace member's manifest anywhere in the tree is enough.
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("deno.json"), "{}").unwrap();
        std::fs::create_dir_all(repo.path().join("packages/api")).unwrap();
        std::fs::write(repo.path().join("packages/api/package.json"), "{}").unwrap();
        assert!(!is_deno_native_project(repo.path()));
    }

    #[test]
    fn package_json_inside_node_modules_does_not_count() {
        // A vendored dependency's manifest is not the project's manifest.
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("deno.json"), "{}").unwrap();
        std::fs::create_dir_all(repo.path().join("node_modules/koa")).unwrap();
        std::fs::write(repo.path().join("node_modules/koa/package.json"), "{}").unwrap();
        assert!(is_deno_native_project(repo.path()));
    }

    #[test]
    fn package_json_inside_build_output_does_not_count() {
        // Build output isn't the project's own manifest — every skip dir
        // beyond node_modules behaves the same way.
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("deno.json"), "{}").unwrap();
        for dir in ["dist", "build", ".next"] {
            std::fs::create_dir_all(repo.path().join(dir)).unwrap();
            std::fs::write(repo.path().join(dir).join("package.json"), "{}").unwrap();
        }
        assert!(is_deno_native_project(repo.path()));
    }

    #[test]
    fn scan_root_named_like_skip_dir_is_still_walked() {
        // A repo directory legitimately named `build` must not be skipped as
        // its own walk root: its nested package.json still counts.
        let parent = tempfile::tempdir().unwrap();
        let repo = parent.path().join("build");
        std::fs::create_dir_all(repo.join("packages/api")).unwrap();
        std::fs::write(repo.join("deno.json"), "{}").unwrap();
        std::fs::write(repo.join("packages/api/package.json"), "{}").unwrap();
        assert!(!is_deno_native_project(&repo));
    }

    #[test]
    fn plain_node_project_is_not_flagged() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("package.json"), "{}").unwrap();
        assert!(!is_deno_native_project(repo.path()));
    }
}
