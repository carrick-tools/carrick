//! A test seam (carrick#3711 R1): the fact layer's files for one service of
//! a tree, built as a scan builds them, for the tests that drive the file
//! orchestrator in process (`FileOrchestrator::analyze_files`). A scan hands
//! the orchestrator these files from the engine
//! ([`crate::agents::file_orchestrator::FactFileSource::later`]); a test that
//! calls the orchestrator itself gets none unless it builds them here.
//!
//! Nothing here is a second path: the rows are the real sidecar's walk, the
//! store is asked through the scan's own ask (answered offline from
//! `<CARRICK_MOCK_FIXTURE_DIR>/library-claims/default.json` under
//! `CARRICK_MOCK_ALL`), and the files come from [`super::first_discovery`].
//! A package the tree does not install, or lists in no public lockfile, is
//! never asked about, as in a scan.

use std::path::Path;

use crate::config::Config;
use crate::request_summary::fact_files::FactFiles;

/// The fact files of the first service `repo/carrick.json` declares (the
/// repo root when it has none), from the rows the sidecar at
/// `sidecar_script` walks into `rows_dir` and the store's first answer.
/// `None` when the service asks the store nothing, as a scan records none.
pub async fn fact_files_of_tree(
    repo: &Path,
    sidecar_script: &Path,
    rows_dir: &Path,
) -> Option<FactFiles> {
    let repo = std::fs::canonicalize(repo).expect("the tree exists");
    let repo_path = repo.to_string_lossy().into_owned();
    let config = match repo.join("carrick.json") {
        file if file.is_file() => Config::load_services(vec![file])
            .expect("the tree's carrick.json reads")
            .into_iter()
            .next()
            .unwrap_or_default(),
        _ => Config::default(),
    };
    let label = config.service_name.as_deref().unwrap_or("(root)");
    let service_root = super::service_sidecar_root(&repo_path, &config);
    let mut files: Vec<std::path::PathBuf> = crate::file_finder::walk_service_quietly(
        &repo_path,
        &config,
        &super::service_ignore_patterns(&config),
    )
    .files;
    files.sort();
    std::fs::create_dir_all(rows_dir).expect("the rows directory is made");
    let out = rows_dir.join("rows.jsonl.gz");
    // Rooted at the tree, as `main` roots a scan's sidecar before the walk
    // lane takes its recipe.
    let sidecar = crate::services::TypeSidecar::spawn(sidecar_script).expect("spawn the sidecar");
    sidecar.set_scan_root(&repo);
    let recipe = sidecar.recipe();
    drop(sidecar);
    crate::position_rows::record_service_rows(
        &recipe,
        &crate::position_rows::ServiceWalk {
            service_root: &service_root,
            tsconfig: config.tsconfig.as_deref(),
            scan_root: &repo,
            files: &files,
            out: &out,
            service: label,
        },
    )
    .expect("the walk writes its rows");
    let positions = crate::position_rows::PositionIndex::of(
        &repo,
        crate::position_rows::read_rows_file(&out).expect("the rows read"),
    );
    let cm: swc_common::sync::Lrc<swc_common::SourceMap> = Default::default();
    let inputs = super::discover_with_positions(
        &repo_path,
        &config,
        cm,
        Some(positions),
        crate::request_summary::ROWS_JOIN_BUDGET,
        true,
        &Default::default(),
        super::Read::Own,
    )
    .expect("discovery reads the service")
    .request_inputs;
    let ask = super::LibraryAsk::start(
        &inputs,
        &super::siblings::Siblings::default(),
        true,
        &service_root,
        &repo,
        Default::default(),
    )?;
    let (early, first) = tokio::sync::oneshot::channel();
    let (_, first) = tokio::join!(super::settled_library_answer(Some(&ask), early), first);
    let first = first.expect("the store's first answer is sent");
    Some(super::first_discovery(
        &ask.sites,
        first,
        &inputs,
        &[],
        &repo,
        &Default::default(),
    ))
}
