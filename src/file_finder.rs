use crate::operation::EndpointProvenance;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const TEST_DIR_NAMES: &[&str] = &[
    "__tests__",
    "__mocks__",
    "__fixtures__",
    "tests",
    "test",
    "fixtures",
    // Storybook config/preview files live under .storybook/ — tooling, not
    // product source (same reasoning as the .stories.* suffixes below).
    ".storybook",
];

const TEST_FILE_SUFFIXES: &[&str] = &[
    ".test.ts",
    ".test.tsx",
    ".spec.ts",
    ".spec.tsx",
    ".test.js",
    ".test.jsx",
    ".spec.js",
    ".spec.jsx",
    // Storybook stories are dev-only component showcases: they never run in
    // production, so any fetch/publish inside one is not a real contract
    // surface — and on UI-heavy repos they are numerous enough to matter for
    // LLM-analysis cost (metamask-extension alone has hundreds).
    ".stories.ts",
    ".stories.tsx",
    ".stories.js",
    ".stories.jsx",
];

/// Directory-name conventions that mark a subtree as mock/test-double code
/// while still being product-adjacent source (so its files ARE scanned, unlike
/// [`TEST_DIR_NAMES`] trees, which `find_files` skips entirely). The canonical
/// case is a mock-service-worker handler tree under `src/mocks/`. These are
/// ecosystem-wide path conventions, deliberately NOT a mock-framework package
/// list — provenance must hold for any registration idiom.
const MOCK_DIR_NAMES: &[&str] = &["mocks", "mock"];

/// Whether any directory segment of `path`, relative to the scanned
/// `root_dir`, matches one of `dir_names` (case-insensitive). Only checked
/// relative to the root so a scan whose root itself sits inside such a
/// directory (e.g. eval fixtures under `tests/fixtures/`) is unaffected;
/// a path outside `root_dir` conservatively returns false.
fn has_dir_named(path: &Path, root_dir: &Path, dir_names: &[&str]) -> bool {
    let relative_path = match path.strip_prefix(root_dir) {
        Ok(p) => p,
        Err(_) => return false,
    };

    relative_path.ancestors().skip(1).any(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                dir_names
                    .iter()
                    .any(|pattern| name.eq_ignore_ascii_case(pattern))
            })
            .unwrap_or(false)
    })
}

fn has_test_dir(path: &Path, root_dir: &Path) -> bool {
    has_dir_named(path, root_dir, TEST_DIR_NAMES)
}

fn has_test_suffix(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            let lower = name.to_ascii_lowercase();
            TEST_FILE_SUFFIXES
                .iter()
                .any(|suffix| lower.ends_with(suffix))
        })
        .unwrap_or(false)
}

fn is_test_path(path: &Path, root_dir: &Path) -> bool {
    has_test_dir(path, root_dir) || has_test_suffix(path)
}

/// Whether any path segment BELOW the scan root matches an ignore pattern
/// exactly. Matching whole segments (not substrings) keeps files like
/// `src/buildkite.ts` or `lib/distances.ts` in scope, and stripping the root
/// first means an explicitly configured scan root named after a pattern
/// (e.g. a service directory `packages/build`) still gets scanned — only its
/// descendants can trigger the ignore.
fn is_ignored(path: &Path, root_dir: &Path, ignore_patterns: &[&str]) -> bool {
    // A path outside the scan root has no segments "below the root" to test;
    // matching against the full (possibly absolute) path would let ignore
    // patterns hit unrelated leading segments.
    let Ok(relative_path) = path.strip_prefix(root_dir) else {
        return false;
    };
    relative_path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|segment| ignore_patterns.contains(&segment))
            .unwrap_or(false)
    })
}

/// Classify where an endpoint's evidence comes from, structurally, from its
/// source path relative to the scanned root: a file under a mock tree
/// ([`MOCK_DIR_NAMES`]) — or any test path per [`is_test_path`], should one
/// ever reach extraction via a custom include — yields
/// [`EndpointProvenance::Mock`]; everything else is a real
/// [`EndpointProvenance::Route`].
///
/// This is the `is_test_path` classification extended to mock trees (#380):
/// test trees are excluded from scanning outright, while mock trees are kept
/// — their handlers often encode the canonical contract — and tagged so
/// matching and the report can say "this producer shape comes from a mock".
///
/// Conservative by construction: a path that cannot be resolved relative to
/// `root_dir` (e.g. an `include` root outside the service directory) is left
/// as `Route` rather than risking segment matches against prefix directories
/// the scan never chose (the same guard `has_test_dir` uses). Known
/// limitation, logged in #380: mock servers living outside a conventionally
/// named tree are not detected; composing structural signals such as
/// dev-dependency placement of the registering import or reachability from
/// runtime entry points is follow-up work.
pub fn endpoint_provenance(path: &Path, root_dir: &Path) -> EndpointProvenance {
    // A path outside the scan root has no segments below the root to
    // classify; is_test_path's suffix check alone would still fire on it,
    // contradicting the conservative guarantee above.
    if path.strip_prefix(root_dir).is_err() {
        return EndpointProvenance::Route;
    }
    if has_dir_named(path, root_dir, MOCK_DIR_NAMES) || is_test_path(path, root_dir) {
        EndpointProvenance::Mock
    } else {
        EndpointProvenance::Route
    }
}

/// Find all JavaScript and TypeScript files in a directory
/// Also looks for carrick.json configuration file
/// Returns (js_ts_files, config_file_option)
///
/// Deliberately does NOT report a `package.json`: the walk visits every
/// manifest below `dir` (test fixtures, examples, nested workers) in
/// filesystem readdir order, so "the" manifest it found was whichever came
/// last on that host. A service's manifest is the one at its root and nowhere
/// else — see [`find_service_files`].
pub fn find_files(dir: &str, ignore_patterns: &[&str]) -> (Vec<PathBuf>, Option<PathBuf>) {
    let mut js_ts_files = Vec::new();
    let mut config_file = None;
    let root_path = Path::new(dir);

    for entry in WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        if is_ignored(path, root_path, ignore_patterns) {
            continue;
        }

        if path.file_name().is_some_and(|name| name == "carrick.json") {
            config_file = Some(path.to_path_buf());
            continue;
        }

        if let Some(extension) = path.extension() {
            let ext_str = extension.to_string_lossy().to_lowercase();
            if matches!(ext_str.as_str(), "js" | "ts" | "jsx" | "tsx") {
                if is_test_path(path, root_path) {
                    continue;
                }
                js_ts_files.push(path.to_path_buf());
            }
        }
    }

    (js_ts_files, config_file)
}

/// Find JS/TS files for a single service, scoped to its `directory` plus any
/// extra `include` source roots (e.g. shared libraries copied in at build
/// time), all relative to `repo_path`. Also returns that service's
/// `package.json`: the file at the service root itself, if present. Nested
/// manifests (fixtures, examples, a sibling service under a `directory: "."`
/// root) are never candidates — a scan must pick the same manifest on every
/// host, and the root is the only one that is unambiguously the service's.
///
/// A service with `directory: None` scopes to the whole repo, so the
/// single-service path behaves exactly like a plain [`find_files`] walk.
pub fn find_service_files(
    repo_path: &str,
    service: &crate::config::Config,
    ignore_patterns: &[&str],
) -> (Vec<PathBuf>, Option<PathBuf>) {
    let root = Path::new(repo_path);
    let service_root = match &service.directory {
        Some(dir) => root.join(dir),
        None => root.to_path_buf(),
    };

    // The carrick.json lives at the repo root, not per service directory, so the
    // config returned here is ignored — config resolution is handled separately.
    let (mut files, _config) = find_files(&service_root.to_string_lossy(), ignore_patterns);

    let manifest = service_root.join("package.json");
    let package_json = manifest.is_file().then_some(manifest);

    for inc in &service.include {
        let inc_path = root.join(inc);
        let (inc_files, _) = find_files(&inc_path.to_string_lossy(), ignore_patterns);
        files.extend(inc_files);
    }

    // An `include` root may overlap the service directory; keep the first
    // occurrence of each file so a file is never parsed twice.
    let mut seen = std::collections::HashSet::new();
    files.retain(|p| seen.insert(p.clone()));

    (files, package_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use tempfile::tempdir;

    #[test]
    fn find_files_skips_test_sources() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("__tests__")).expect("__tests__ dir");

        let source_file = root.join("src").join("app.ts");
        let test_file = root.join("__tests__").join("app.spec.ts");
        let config_path = root.join("carrick.json");
        let package_path = root.join("package.json");

        File::create(&source_file).expect("source file");
        File::create(&test_file).expect("test file");
        File::create(&config_path).expect("config file");
        File::create(&package_path).expect("package file");

        let (files, config) = find_files(root.to_str().unwrap(), &[]);

        assert_eq!(files, vec![source_file]);
        assert_eq!(config, Some(config_path));
    }

    #[test]
    fn find_files_skips_suffixes_and_fixture_dirs() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("services")).expect("services dir");
        fs::create_dir_all(root.join("fixtures").join("seeds")).expect("fixtures dir");

        let normal_file = root.join("services").join("handler.tsx");
        let suffix_file = root.join("services").join("handler.test.tsx");
        let fixture_file = root.join("fixtures").join("seeds").join("seed.ts");
        let config_path = root.join("carrick.json");
        let package_path = root.join("package.json");

        File::create(&normal_file).expect("normal file");
        File::create(&suffix_file).expect("suffix file");
        File::create(&fixture_file).expect("fixture file");
        File::create(&config_path).expect("config file");
        File::create(&package_path).expect("package file");

        let (files, config) = find_files(root.to_str().unwrap(), &[]);

        assert_eq!(files, vec![normal_file]);
        assert_eq!(config, Some(config_path));
    }

    #[test]
    fn find_files_skips_storybook_stories_and_config() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src").join("components")).expect("components dir");
        fs::create_dir_all(root.join(".storybook")).expect(".storybook dir");

        let component = root.join("src").join("components").join("Button.tsx");
        let story_tsx = root
            .join("src")
            .join("components")
            .join("Button.stories.tsx");
        let story_ts = root.join("src").join("components").join("api.stories.ts");
        let sb_config = root.join(".storybook").join("preview.tsx");
        // "stories" in the basename without the dot-suffix shape stays in.
        let real_module = root.join("src").join("components").join("stories.ts");

        for f in [&component, &story_tsx, &story_ts, &sb_config, &real_module] {
            File::create(f).expect("file");
        }

        let (mut files, _) = find_files(root.to_str().unwrap(), &[]);
        files.sort();

        let mut expected = vec![component, real_module];
        expected.sort();
        assert_eq!(files, expected);
    }

    #[test]
    fn find_files_allows_root_dir_matching_test_pattern() {
        let tmp = tempdir().expect("temp dir");
        // Create a root directory named "fixtures" which is normally excluded
        let root = tmp.path().join("fixtures");

        fs::create_dir_all(&root).expect("fixtures dir");
        fs::create_dir_all(root.join("src")).expect("src dir");

        let source_file = root.join("src").join("app.ts");
        let config_path = root.join("carrick.json");
        let package_path = root.join("package.json");

        File::create(&source_file).expect("source file");
        File::create(&config_path).expect("config file");
        File::create(&package_path).expect("package file");

        // Pass the "fixtures" directory as the root
        let (files, config) = find_files(root.to_str().unwrap(), &[]);

        assert_eq!(files, vec![source_file]);
        assert_eq!(config, Some(config_path));
    }

    const ARTIFACT_IGNORES: &[&str] = &["node_modules", "dist", "build", ".next"];

    #[test]
    fn find_files_ignore_matches_segments_not_substrings() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("lib")).expect("lib dir");
        fs::create_dir_all(root.join("builder")).expect("builder dir");

        // Basenames and directory names that merely contain an ignore pattern
        // as a substring must still be scanned.
        let substring_in_basename = root.join("src").join("buildkite.ts");
        let substring_in_basename_2 = root.join("lib").join("distances.ts");
        let substring_in_dir = root.join("builder").join("plan.ts");

        for f in [
            &substring_in_basename,
            &substring_in_basename_2,
            &substring_in_dir,
        ] {
            File::create(f).expect("file");
        }

        let (mut files, _) = find_files(root.to_str().unwrap(), ARTIFACT_IGNORES);
        files.sort();

        let mut expected = vec![
            substring_in_basename,
            substring_in_basename_2,
            substring_in_dir,
        ];
        expected.sort();
        assert_eq!(files, expected);
    }

    #[test]
    fn find_files_ignores_artifact_dirs_below_root() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("node_modules").join("dep")).expect("node_modules dir");
        fs::create_dir_all(root.join("dist")).expect("dist dir");
        fs::create_dir_all(root.join("build")).expect("build dir");
        fs::create_dir_all(root.join(".next").join("server")).expect(".next dir");

        let kept = root.join("src").join("app.ts");
        File::create(&kept).expect("kept file");
        File::create(root.join("node_modules/dep/index.ts")).expect("dep file");
        File::create(root.join("node_modules/dep/package.json")).expect("dep package");
        File::create(root.join("dist/app.js")).expect("dist file");
        File::create(root.join("build/app.js")).expect("build file");
        File::create(root.join(".next/server/page.js")).expect(".next file");

        let (files, _) = find_files(root.to_str().unwrap(), ARTIFACT_IGNORES);

        assert_eq!(files, vec![kept]);
    }

    #[test]
    fn find_files_allows_root_dir_named_like_ignore_pattern() {
        let tmp = tempdir().expect("temp dir");
        // The explicitly configured scan root is named after an ignore
        // pattern; only descendants below the root may trigger the ignore.
        let root = tmp.path().join("packages").join("build");

        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("node_modules").join("dep")).expect("node_modules dir");

        let kept = root.join("src").join("index.ts");
        File::create(&kept).expect("kept file");
        File::create(root.join("node_modules/dep/index.ts")).expect("dep file");

        let (files, _) = find_files(root.to_str().unwrap(), ARTIFACT_IGNORES);

        assert_eq!(files, vec![kept]);
    }

    #[test]
    fn find_service_files_scans_service_root_named_like_ignore_pattern() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("packages/build/src")).expect("service dir");
        fs::create_dir_all(root.join("packages/build/node_modules/dep")).expect("dep dir");

        let kept = root.join("packages/build/src/index.ts");
        let pkg = root.join("packages/build/package.json");
        File::create(&kept).expect("kept file");
        File::create(&pkg).expect("package file");
        File::create(root.join("packages/build/node_modules/dep/index.ts")).expect("dep file");

        let service = Config {
            directory: Some("packages/build".to_string()),
            ..Default::default()
        };
        let (files, package) =
            find_service_files(root.to_str().unwrap(), &service, ARTIFACT_IGNORES);

        assert_eq!(files, vec![kept]);
        assert_eq!(package, Some(pkg));
    }

    #[test]
    fn endpoint_provenance_tags_mock_trees_and_test_paths() {
        use crate::operation::EndpointProvenance;

        let root = Path::new("");
        // The observed real-world shape: an MSW-style handler tree.
        assert_eq!(
            endpoint_provenance(Path::new("src/mocks/handlers.ts"), root),
            EndpointProvenance::Mock
        );
        // Case-insensitive segment match, any depth, singular variant.
        assert_eq!(
            endpoint_provenance(Path::new("src/testing/Mock/server.ts"), root),
            EndpointProvenance::Mock
        );
        // Test-path classification is folded in (defensive: such files are
        // normally excluded from scanning entirely).
        assert_eq!(
            endpoint_provenance(Path::new("src/__mocks__/api.ts"), root),
            EndpointProvenance::Mock
        );
        assert_eq!(
            endpoint_provenance(Path::new("src/api/server.spec.ts"), root),
            EndpointProvenance::Mock
        );
        // Real product routes stay routes.
        assert_eq!(
            endpoint_provenance(Path::new("src/routes/users.ts"), root),
            EndpointProvenance::Route
        );
        // A FILE named after the convention is not a mock tree.
        assert_eq!(
            endpoint_provenance(Path::new("src/api/mocks.ts"), root),
            EndpointProvenance::Route
        );
    }

    #[test]
    fn endpoint_provenance_outside_scan_root_stays_route() {
        // A test-suffixed file OUTSIDE the scan root must not classify as
        // Mock via the suffix check alone.
        assert_eq!(
            endpoint_provenance(Path::new("elsewhere/foo.spec.ts"), Path::new("service")),
            EndpointProvenance::Route
        );
    }

    #[test]
    fn endpoint_provenance_is_relative_to_scan_root() {
        use crate::operation::EndpointProvenance;

        // A scan rooted INSIDE a mocks/tests prefix (e.g. eval fixtures under
        // tests/fixtures/) must not tag everything mock: only segments below
        // the root count.
        let root = Path::new("tests/fixtures/mocks/repo-a");
        assert_eq!(
            endpoint_provenance(Path::new("tests/fixtures/mocks/repo-a/src/app.ts"), root),
            EndpointProvenance::Route
        );
        assert_eq!(
            endpoint_provenance(
                Path::new("tests/fixtures/mocks/repo-a/src/mocks/handlers.ts"),
                root
            ),
            EndpointProvenance::Mock
        );
        // A path outside the root cannot be classified — conservative Route.
        assert_eq!(
            endpoint_provenance(Path::new("elsewhere/mocks/handlers.ts"), root),
            EndpointProvenance::Route
        );
    }

    #[test]
    fn find_service_files_scopes_to_directory() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        // Two services in one repo, plus a file at the root that belongs to
        // neither service's directory.
        fs::create_dir_all(root.join("svc-a/src")).expect("svc-a dir");
        fs::create_dir_all(root.join("svc-b/src")).expect("svc-b dir");

        let a_file = root.join("svc-a/src/index.ts");
        let a_pkg = root.join("svc-a/package.json");
        let b_file = root.join("svc-b/src/index.ts");
        let root_file = root.join("root.ts");

        File::create(&a_file).expect("a file");
        File::create(&a_pkg).expect("a package");
        File::create(&b_file).expect("b file");
        File::create(&root_file).expect("root file");

        let service = Config {
            directory: Some("svc-a".to_string()),
            ..Default::default()
        };
        let (files, package) = find_service_files(root.to_str().unwrap(), &service, &[]);

        assert_eq!(files, vec![a_file]);
        assert_eq!(package, Some(a_pkg));
    }

    #[test]
    fn find_service_files_merges_include_roots_and_dedups() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("lambdas/check-or-upload")).expect("svc dir");
        fs::create_dir_all(root.join("lambdas/_shared")).expect("shared dir");

        let svc_file = root.join("lambdas/check-or-upload/index.ts");
        let shared_file = root.join("lambdas/_shared/log.ts");
        File::create(&svc_file).expect("svc file");
        File::create(&shared_file).expect("shared file");

        let service = Config {
            directory: Some("lambdas/check-or-upload".to_string()),
            // Include the shared root twice to prove de-duplication.
            include: vec!["lambdas/_shared".to_string(), "lambdas/_shared".to_string()],
            ..Default::default()
        };
        let (mut files, _) = find_service_files(root.to_str().unwrap(), &service, &[]);
        files.sort();

        let mut expected = vec![svc_file, shared_file];
        expected.sort();
        assert_eq!(files, expected);
    }

    #[test]
    fn find_service_files_whole_repo_matches_find_files() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("src dir");
        File::create(root.join("src/app.ts")).expect("source file");
        File::create(root.join("package.json")).expect("package file");

        // directory: None => whole-repo scope, identical to find_files.
        let service = Config::default();
        let (svc_files, svc_pkg) = find_service_files(root.to_str().unwrap(), &service, &[]);
        let (files, _) = find_files(root.to_str().unwrap(), &[]);

        assert_eq!(svc_files, files);
        assert_eq!(svc_pkg, Some(root.join("package.json")));
    }

    // The manifest is the one at the service root — never one found by
    // walking. Before this, the walk assigned every package.json it met, so a
    // service got whichever manifest its host's readdir order visited last:
    // a repo scanned from macOS and from ubuntu-latest indexed different
    // dependency sets for the same commit.

    #[test]
    fn find_service_files_ignores_nested_fixture_manifest() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("svc/src")).expect("src dir");
        fs::create_dir_all(root.join("svc/test/fixtures/installed")).expect("fixture dir");
        File::create(root.join("svc/src/app.ts")).expect("source file");
        let root_pkg = root.join("svc/package.json");
        File::create(&root_pkg).expect("root package");
        File::create(root.join("svc/test/fixtures/installed/package.json"))
            .expect("fixture package");

        let service = Config {
            directory: Some("svc".to_string()),
            ..Default::default()
        };
        let (_, package) = find_service_files(root.to_str().unwrap(), &service, &[]);

        assert_eq!(package, Some(root_pkg));
    }

    #[test]
    fn find_service_files_ignores_sibling_service_manifest_under_repo_root() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        // A root service (`directory: "."`) whose tree also contains a second
        // service's directory — the site + its worker layout.
        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("workers/proxy")).expect("worker dir");
        File::create(root.join("src/app.ts")).expect("source file");
        let root_pkg = root.join("package.json");
        File::create(&root_pkg).expect("root package");
        File::create(root.join("workers/proxy/package.json")).expect("worker package");

        let service = Config {
            directory: Some(".".to_string()),
            ..Default::default()
        };
        let (_, package) = find_service_files(root.to_str().unwrap(), &service, &[]);

        assert_eq!(package, Some(root_pkg));
    }

    #[test]
    fn find_service_files_reports_no_manifest_when_root_has_none() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        // Only a nested manifest exists: it must not be promoted to the
        // service's own.
        fs::create_dir_all(root.join("svc/examples/demo")).expect("example dir");
        File::create(root.join("svc/examples/demo/package.json")).expect("example package");

        let service = Config {
            directory: Some("svc".to_string()),
            ..Default::default()
        };
        let (_, package) = find_service_files(root.to_str().unwrap(), &service, &[]);

        assert_eq!(package, None);
    }
}
