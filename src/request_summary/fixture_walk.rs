//! One service of a fixture tree under `tests/fixtures/`, read by discovery
//! with the rows the real sidecar walks or with none, as a run without
//! `CARRICK_POSITION_ROWS` (carrick#2413). Shared by the tests of what the
//! summaries read through the rows.

use super::*;
use crate::config::Config;
use crate::position_rows::PositionIndex;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The sidecar script, when node and the built sidecar are here. CI builds
/// it before the Rust tests, so a skip there would mean nothing.
pub(crate) fn sidecar() -> Option<PathBuf> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    let node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok();
    if script.exists() && node {
        return Some(script);
    }
    assert!(
        std::env::var("CI").is_err(),
        "node or the built sidecar is missing in CI"
    );
    crate::errln!("Skipping: node or the built sidecar is missing");
    None
}

/// An edit of the wrapper fixture's config (`position-rows-wrapper`): its own
/// config reads the extensionless imports under bundler resolution. Under
/// NodeNext they do not resolve (the package is `"type": "module"`), which is
/// the unresolved import the tests of the call-graph fallback need.
pub(crate) const NODENEXT: (&str, &str) = (
    "web/tsconfig.json",
    r#"{ "compilerOptions": { "target": "ES2020", "module": "NodeNext", "moduleResolution": "NodeNext", "jsx": "react-jsx", "strict": true }, "include": ["src"] }"#,
);

/// The member declarations the trace's own verify request returns for one
/// service of a real repo, asked of the built sidecar from the service's
/// root as a scan asks it (carrick#2474 R1, R2b): the offline censuses'.
pub(crate) fn census_declarations(
    script: &Path,
    root: &Path,
    service: &Config,
    inputs: &RequestSummaryInputs,
    siblings: &[&RequestSummaryInputs],
    exports: &[crate::library_claims::ExportClaims],
) -> super::discovery::MemberDeclarations {
    let name = service.service_name.as_deref().unwrap_or("(root)");
    let service_root = root.join(service.directory.as_deref().unwrap_or("."));
    let sidecar = crate::services::TypeSidecar::spawn(script).expect("spawn the sidecar");
    sidecar.set_scan_root(root);
    sidecar.start_init(&service_root, None);
    sidecar
        .wait_ready(std::time::Duration::from_secs(900))
        .expect("the sidecar is ready");
    let ask = super::discovery::declaration_ask(inputs, siblings, exports);
    let verdicts = sidecar
        .verify_library_claims(&service_root, None, &ask.checks)
        .map(|answer| answer.verdicts)
        .unwrap_or_default();
    let declarations = super::discovery::MemberDeclarations::of(&ask, &verdicts);
    crate::outln!(
        "  {name}: {} check(s), {} not reached, {} declaration(s), {} scope(s) verified",
        declarations.checks,
        declarations.not_reached,
        declarations.len(),
        declarations.scope_answers.len()
    );
    declarations
}

/// A fixture service, read.
pub(crate) struct Walked {
    _dir: tempfile::TempDir,
    /// The copied repo's root, canonical.
    pub(crate) repo: PathBuf,
    pub(crate) inputs: RequestSummaryInputs,
    /// How long discovery took, rows bound included (not the walk).
    pub(crate) discovered_in: std::time::Duration,
}

impl Walked {
    /// The path discovery read `file` (relative to the repo root) under.
    pub(crate) fn path(&self, file: &str) -> PathBuf {
        let wanted = self.repo.join(file);
        self.inputs
            .files
            .keys()
            .find(|path| **path == wanted)
            .cloned()
            .unwrap_or_else(|| panic!("{file} is not read"))
    }
}

/// `tests/fixtures/<fixture>/repo`'s first service, rooted at `service_dir`
/// and named `service`, with `edit` applied (paths under `service_dir`,
/// directories made as needed; empty text removes the file).
pub(crate) fn walked(
    fixture: &str,
    service_dir: &str,
    service: &str,
    edit: &[(&str, &str)],
    with_rows: bool,
) -> Option<Walked> {
    walked_within(
        fixture,
        service_dir,
        service,
        edit,
        with_rows,
        super::ROWS_JOIN_BUDGET,
    )
}

/// Builds an index over a copied repo's root.
pub(crate) type IndexOf = fn(&Path) -> PositionIndex;

/// Where a fixture service's rows come from.
#[derive(Clone, Copy)]
pub(crate) enum Rows {
    /// The real sidecar's walk.
    Walked,
    /// No index at all (`positions: None`).
    Absent,
    /// An index built over the copied repo's root, as a lost walk or no walk
    /// leaves one (carrick#4256).
    Index(IndexOf),
}

impl Rows {
    fn of(with_rows: bool) -> Self {
        if with_rows {
            Self::Walked
        } else {
            Self::Absent
        }
    }
}

/// [`walked`], binding the rows within `budget` (carrick#2564).
pub(crate) fn walked_within(
    fixture: &str,
    service_dir: &str,
    service: &str,
    edit: &[(&str, &str)],
    with_rows: bool,
    budget: std::time::Duration,
) -> Option<Walked> {
    walked_with(
        fixture,
        service_dir,
        service,
        edit,
        Rows::of(with_rows),
        budget,
    )
}

/// [`walked`], with the service's rows from `rows` (carrick#4256).
pub(crate) fn walked_over(
    fixture: &str,
    service_dir: &str,
    service: &str,
    edit: &[(&str, &str)],
    rows: Rows,
) -> Option<Walked> {
    walked_with(
        fixture,
        service_dir,
        service,
        edit,
        rows,
        super::ROWS_JOIN_BUDGET,
    )
}

fn walked_with(
    fixture: &str,
    service_dir: &str,
    service: &str,
    edit: &[(&str, &str)],
    rows: Rows,
    budget: std::time::Duration,
) -> Option<Walked> {
    let script = sidecar()?;
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap().join("repo");
    copy_tree(
        &manifest.join("tests/fixtures").join(fixture).join("repo"),
        &repo,
    );
    let service_root = repo.join(service_dir);
    for (name, text) in edit {
        let path = service_root.join(name);
        if text.is_empty() {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
    }
    // The service named `service`, else the first.
    let mut services = Config::load_services(vec![repo.join("carrick.json")]).unwrap();
    let at = services
        .iter()
        .position(|config| config.service_name.as_deref() == Some(service))
        .unwrap_or(0);
    let config = services.remove(at);
    let positions = match rows {
        Rows::Walked => Some(rows_of(
            &script,
            dir.path(),
            &repo,
            &service_root,
            &config,
            service,
        )),
        Rows::Absent => None,
        Rows::Index(index) => Some(index(&repo)),
    };
    let started = std::time::Instant::now();
    let inputs = crate::engine::discover_service_inputs_within(&repo, &config, positions, budget);
    Some(Walked {
        _dir: dir,
        repo,
        inputs,
        discovered_in: started.elapsed(),
    })
}

/// The rows the real sidecar walks for `service` of the copied tree `repo`,
/// written beside it under `dir`.
fn rows_of(
    script: &Path,
    dir: &Path,
    repo: &Path,
    service_root: &Path,
    config: &Config,
    service: &str,
) -> PositionIndex {
    let mut files = crate::file_finder::walk_service(&repo.to_string_lossy(), config, &[]).files;
    files.sort();
    // A directory beside the tree: the walk refuses to write into it.
    let positions = dir.join("positions");
    std::fs::create_dir_all(&positions).unwrap();
    let out = positions.join(format!("repo__{service}.jsonl.gz"));
    let sidecar = crate::services::TypeSidecar::spawn(script).expect("spawn the sidecar");
    sidecar.set_scan_root(repo);
    let recipe = sidecar.recipe();
    drop(sidecar);
    crate::position_rows::record_service_rows(
        &recipe,
        &crate::position_rows::ServiceWalk {
            service_root,
            tsconfig: Some("tsconfig.json"),
            scan_root: repo,
            files: &files,
            out: &out,
            service,
        },
    )
    .expect("the walk writes its rows");
    PositionIndex::of(
        repo,
        crate::position_rows::read_rows_file(&out).expect("the rows read"),
    )
}

/// Several services of one fixture tree, each read with its own rows or
/// with none (carrick#2618).
pub(crate) struct WalkedServices {
    _dir: tempfile::TempDir,
    /// The copied repo's root, canonical.
    pub(crate) repo: PathBuf,
    /// Each named service's inputs, in the order named.
    pub(crate) inputs: Vec<RequestSummaryInputs>,
}

/// `tests/fixtures/<fixture>/repo`'s services `services` (directory under
/// the repo and name, as `carrick.json` declares them), copied once and
/// each walked with its own rows.
pub(crate) fn walked_services(
    fixture: &str,
    services: &[(&str, &str)],
    with_rows: bool,
) -> Option<WalkedServices> {
    walked_services_over(fixture, services, with_rows, None)
}

/// [`walked_services`], the service named in `instead` read over the index
/// its function builds from the repo root rather than walked (carrick#4256).
pub(crate) fn walked_services_over(
    fixture: &str,
    services: &[(&str, &str)],
    with_rows: bool,
    instead: Option<(&str, IndexOf)>,
) -> Option<WalkedServices> {
    let script = sidecar()?;
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap().join("repo");
    copy_tree(
        &manifest.join("tests/fixtures").join(fixture).join("repo"),
        &repo,
    );
    let configs = Config::load_services(vec![repo.join("carrick.json")]).unwrap();
    let inputs = services
        .iter()
        .map(|(service_dir, service)| {
            let config = configs
                .iter()
                .find(|config| config.service_name.as_deref() == Some(*service))
                .unwrap_or_else(|| panic!("carrick.json declares no service {service}"));
            let positions = if let Some((name, index)) = instead
                && name == *service
            {
                Some(index(&repo))
            } else {
                with_rows.then(|| {
                    rows_of(
                        &script,
                        dir.path(),
                        &repo,
                        &repo.join(service_dir),
                        config,
                        service,
                    )
                })
            };
            crate::engine::discover_service_inputs_within(
                &repo,
                config,
                positions,
                super::ROWS_JOIN_BUDGET,
            )
        })
        .collect();
    Some(WalkedServices {
        _dir: dir,
        repo,
        inputs,
    })
}

/// What the real sidecar finds for each call of `walked` through an interface
/// member, asked as a scan asks it: of a process initialised on the service's
/// directory (carrick#2432 slice B).
pub(crate) fn implementations_of(
    script: &Path,
    walked: &Walked,
    service_dir: &str,
) -> super::implementations::Implementations {
    let sidecar = crate::services::TypeSidecar::spawn(script).expect("spawn the sidecar");
    sidecar.set_scan_root(&walked.repo);
    sidecar.start_init(&walked.repo.join(service_dir), None);
    sidecar
        .wait_ready(std::time::Duration::from_secs(900))
        .expect("the sidecar is ready");
    super::implementations::implementations_of(&walked.inputs, |queries| {
        sidecar.implementations(queries, &walked.repo)
    })
}
