//! Whether a tree is prepared enough to be worth scanning (carrick#1254).
//!
//! A scan of an unprepared checkout costs the same as a scan of a prepared
//! one and produces a thinner index: every type that resolves through a
//! package or through a generated module bottoms out at `any`, and nothing in
//! the result says why. On one monorepo, installing the dependencies moved
//! untyped routes 147 -> 116, and a single mapping to a generated client that
//! the checkout did not hold accounted for 83 more rows. Both are config reads
//! and a `stat`, and both are knowable before the first model call.
//!
//! So this module answers one question — *is this tree prepared* — and the
//! scan refuses when it is not. Two signals, per service, neither of which
//! catches the other:
//!
//! * **Dependencies not installed.** A lockfile reachable from the service
//!   root states that this tree is meant to have an install; no `node_modules`
//!   anywhere from the service root up to that lockfile's directory says it
//!   does not have one. The refusal names the command, because the lockfile
//!   names the package manager.
//! * **A mapping whose target directory is not on disk.** The service's own
//!   config maps a specifier to a path, and the path is not there. The refusal
//!   names the mapping and the missing directory and stops: nothing in config
//!   says what fills a generated directory, and guessing would mean a list of
//!   generator names.
//!
//! **Per service, and "reachable from" the service root.** A repo-level check
//! passes a tree whose root is installed and whose nested workspace is not,
//! which is the exact shape that produced the measurement above.
//!
//! **The refusal is the default and [`ALLOW_FLAG`] is the way out.** A CI job
//! that checks out without installing is a legitimate, deliberate bare scan;
//! it just has to say so. Both signals are proxies — a declared-but-unused
//! mapping is refused for nothing, a stale install passes — and neither is
//! survivable under a check that cannot be overridden.
//!
//! The user-facing statement of all this is the README's "An unprepared
//! checkout is refused" section, under Dependencies.
//!
//! What it deliberately does not do: install anything, run anything, or refuse
//! a tree that never stated an install. A manifest with no lockfile above it
//! is the same tree the GitHub Action's own installer declines to prepare, and
//! refusing it would fail a scan for a state Carrick chose not to fix.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::warn;

use crate::config::Config;

/// The flag that scans an unprepared tree deliberately.
pub const ALLOW_FLAG: &str = "--allow-unprepared";

/// The same override as an environment variable, for a pipeline that sets the
/// scan up rather than spelling the command, and for the parent of a detached
/// or per-repo build to hand down to the child that does the scanning.
pub const ALLOW_ENV: &str = "CARRICK_ALLOW_UNPREPARED";

/// Lockfiles, and the command that installs what each of them locks. The list
/// is the check's whole knowledge of package managers: a name here is a file
/// on disk that states an install, not a framework or a library.
const LOCKFILES: [(&str, &str); 3] = [
    ("pnpm-lock.yaml", "pnpm install"),
    ("yarn.lock", "yarn install"),
    ("package-lock.json", "npm install"),
];

/// The Deno lockfile and its install command, kept apart from [`LOCKFILES`]
/// because a Deno service reaches it through a different rule: Deno caches
/// outside the tree unless its config asks for a local `node_modules`.
const DENO_LOCKFILE: (&str, &str) = ("deno.lock", "deno install");

/// How many unprepared services the refusal names before it counts the rest,
/// matching the other refusals a build prints.
const MAX_NAMED: usize = 5;

/// Whether [`ALLOW_FLAG`] was passed to this process.
static ALLOWED: AtomicBool = AtomicBool::new(false);

/// Record that this process was given [`ALLOW_FLAG`]. Called once, from
/// argument parsing.
pub fn allow(flag: bool) {
    if flag {
        ALLOWED.store(true, Ordering::Relaxed);
    }
}

/// Whether this process was told to scan an unprepared tree anyway, by the
/// flag or by the environment.
///
/// The environment is what a CI job sets and what a build hands to the scans
/// it spawns, so a flag typed once at the top of a workspace build reaches
/// every process that would otherwise refuse.
pub fn allowed() -> bool {
    ALLOWED.load(Ordering::Relaxed)
        || std::env::var(ALLOW_ENV)
            .map(|value| !value.is_empty() && value != "0" && value != "false")
            .unwrap_or(false)
}

/// One service, and the one thing about it that is not prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unprepared {
    /// A lockfile states an install this tree has not had.
    Dependencies {
        service: String,
        /// Repo-relative directory the lockfile sits in, which is where the
        /// command runs.
        install_root: String,
        lockfile: &'static str,
        command: &'static str,
    },
    /// A config mapping names a directory that is not on the checkout.
    Mapping {
        service: String,
        /// The mapping key as written in the config.
        declared_by: String,
        /// Repo-relative directory the mapping names.
        target: String,
    },
}

impl Unprepared {
    /// Put the repo in front of the service name, for a build that scans more
    /// than one and would otherwise print two unrelated `api`s.
    pub fn in_repo(&mut self, repo: &str) {
        let renamed = |service: &str| format!("{repo}/{service}");
        match self {
            Unprepared::Dependencies { service, .. } | Unprepared::Mapping { service, .. } => {
                *service = renamed(service);
            }
        }
    }

    /// The line a user acts on: what is missing, and the command that fixes it
    /// when there is one to name.
    pub fn sentence(&self) -> String {
        match self {
            Unprepared::Dependencies {
                service,
                install_root,
                lockfile,
                command,
            } => format!(
                "`{service}`: dependencies are not installed. {lockfile} in {} states them and no \
                 node_modules is reachable from the service root, so every type that resolves \
                 through a package would be `any`. Run `{command}` in {}.",
                display_dir(install_root),
                display_dir(install_root)
            ),
            Unprepared::Mapping {
                service,
                declared_by,
                target,
            } => format!(
                "`{service}`: its config maps `{declared_by}` to {target}, and that directory is \
                 not on this checkout, so every type through it would be `any`. Nothing in the \
                 config says what fills it — run whatever generates it, or drop the mapping."
            ),
        }
    }
}

/// Refuse unless every service is prepared, or the override says otherwise.
///
/// The one entry point a scan calls. It is `Err` with the whole refusal
/// message, so a caller reports it the way it reports any other reason it did
/// not scan.
pub fn require_prepared(repo_root: &Path, services: &[Config]) -> Result<(), String> {
    match refusal(unprepared(repo_root, services)) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// The message for a set of unprepared services, or `None` when there are
/// none and when the override is set.
///
/// The override is read here rather than at each call site so that the check
/// itself always runs: a run that would have been refused still knows it,
/// which is what lets an allowed run say what it is scanning over.
pub fn refusal(found: Vec<Unprepared>) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    if allowed() {
        for row in &found {
            warn!(
                "Scanning an unprepared tree, because {ALLOW_ENV} is set: {}",
                row.sentence()
            );
        }
        return None;
    }
    let mut lines = vec![format!(
        "this checkout is not prepared to be scanned, and a scan of it would index `any` where a \
         package or a generated module should be. {} service(s):",
        found.len()
    )];
    lines.extend(found.iter().take(MAX_NAMED).map(|row| {
        let mut line = String::from("  ");
        line.push_str(&row.sentence());
        line
    }));
    if found.len() > MAX_NAMED {
        lines.push(format!("  and {} more", found.len() - MAX_NAMED));
    }
    lines.push(format!(
        "Then run this again. To scan the checkout exactly as it is — which is what a CI job that \
         deliberately does not install should do — pass {ALLOW_FLAG} or set {ALLOW_ENV}=1."
    ));
    Some(lines.join("\n"))
}

/// Every service of this repo that is not prepared, in the order the config
/// declares them, dependencies before mappings for one service.
pub fn unprepared(repo_root: &Path, services: &[Config]) -> Vec<Unprepared> {
    let mut found = Vec::new();
    for service in services {
        let name = service
            .service_name
            .clone()
            .unwrap_or_else(|| "this repo".to_string());
        let directory = PathBuf::from(service.directory.as_deref().unwrap_or("."));
        if let Some(row) = dependencies_unprepared(repo_root, service, &directory, &name) {
            found.push(row);
        }
        found.extend(missing_mappings(repo_root, service, &directory, &name));
    }
    found
}

/// A lockfile above the service root with no `node_modules` under it.
///
/// A Deno service is checked only when its config asks for a local
/// `node_modules`: Deno otherwise caches outside the tree, where an absent
/// directory says nothing at all. That is the same rule the type sidecar
/// applies when it marks a capture as taken on a bare checkout.
fn dependencies_unprepared(
    repo_root: &Path,
    service: &Config,
    directory: &Path,
    name: &str,
) -> Option<Unprepared> {
    let service_root = service_root(repo_root, directory);
    let deno = crate::deno_support::service_manifest(repo_root, service);
    // A Deno service that has not asked for a local `node_modules` keeps its
    // dependencies outside the tree, where their absence from it means
    // nothing. The same rule the type sidecar applies before it calls a
    // capture bare.
    let candidates: &[(&'static str, &'static str)] = match &deno {
        Some(config) if !deno_wants_node_modules(config) => return None,
        Some(_) => &[DENO_LOCKFILE],
        None => &LOCKFILES,
    };
    // The install root is the nearest directory at or above the service that
    // states an install, and nothing above it is asked about: a monorepo root
    // with its own lockfile and its own `node_modules` is not an install of a
    // nested workspace that has a lockfile of its own.
    let (install_root, lockfile, command) =
        ancestors_within(&service_root, repo_root).find_map(|dir| {
            candidates
                .iter()
                .find(|(lock, _)| dir.join(lock).is_file())
                .map(|(lock, command)| (dir.clone(), *lock, *command))
        })?;
    // An install that would install nothing is not one worth refusing over.
    if !declares_dependencies(&service_root, &install_root) {
        return None;
    }
    if ancestors_within(&service_root, &install_root).any(|dir| dir.join("node_modules").is_dir()) {
        return None;
    }
    Some(Unprepared::Dependencies {
        service: name.to_string(),
        install_root: relative(&install_root, repo_root),
        lockfile,
        command,
    })
}

/// Whether anything from the service root up to the install root declares a
/// dependency to install.
fn declares_dependencies(service_root: &Path, install_root: &Path) -> bool {
    ancestors_within(service_root, install_root).any(|dir| {
        let declared = |manifest: PathBuf| {
            crate::packages::read_manifest(&manifest)
                .ok()
                .is_some_and(|facts| {
                    !facts.package.dependencies.is_empty()
                        || !facts.package.dev_dependencies.is_empty()
                        || !facts.package.peer_dependencies.is_empty()
                        || !facts.package.optional_dependencies.is_empty()
                })
        };
        declared(dir.join("package.json"))
            || crate::deno_support::manifest_at(&dir).is_some_and(declared)
    })
}

/// `nodeModulesDir` stated as a local directory, in the config itself. Deno's
/// own default is left alone: a project that says nothing caches outside the
/// tree as far as this check is concerned, so an absent `node_modules` is not
/// evidence of anything.
fn deno_wants_node_modules(config: &Path) -> bool {
    crate::packages::read_json_config(config)
        .ok()
        .and_then(|json| json.get("nodeModulesDir").cloned())
        .is_some_and(|value| match value {
            serde_json::Value::Bool(local) => local,
            serde_json::Value::String(mode) => mode == "auto" || mode == "manual",
            _ => false,
        })
}

/// Mappings the configs governing this service root declare, whose target
/// directory is not on the checkout.
///
/// The configs read are the ones at or above the service root, which is what
/// governs the service's own imports; the same [`crate::module_aliases`]
/// reader the call graph uses answers what each mapping names, so a mapping
/// here and a mapping in the scan's own unresolved-import report are the same
/// thing.
fn missing_mappings(
    repo_root: &Path,
    service: &Config,
    directory: &Path,
    name: &str,
) -> Vec<Unprepared> {
    let configs = governing_configs(repo_root, directory);
    let service_tsconfig = service
        .tsconfig
        .as_deref()
        .map(|tsconfig| (directory, Path::new(tsconfig)));
    let aliases = crate::module_aliases::ModuleAliases::build(
        repo_root,
        &configs,
        &BTreeMap::new(),
        service_tsconfig,
    );
    aliases
        .claimed_directories()
        .into_iter()
        .filter(|claim| {
            !claim
                .directories
                .iter()
                .any(|dir| repo_root.join(dir).is_dir())
        })
        .filter_map(|claim| {
            // The smallest of the claimed directories, so two runs of the same
            // tree print the same sentence whatever order the configs were
            // read in.
            let target = claim.directories.first()?;
            Some(Unprepared::Mapping {
                service: name.to_string(),
                declared_by: claim.declared_by,
                target: display_dir(&target.to_string_lossy()),
            })
        })
        .collect()
}

/// Config files at or above the service root, repo-relative, nearest last so
/// the alias reader's own precedence decides between them.
fn governing_configs(repo_root: &Path, directory: &Path) -> Vec<PathBuf> {
    let service_root = service_root(repo_root, directory);
    let mut configs: Vec<PathBuf> = ancestors_within(&service_root, repo_root)
        .flat_map(|dir| {
            [
                "tsconfig.json",
                "jsconfig.json",
                "package.json",
                "deno.json",
                "deno.jsonc",
            ]
            .into_iter()
            .map(move |name| dir.join(name))
        })
        .filter(|path| path.is_file())
        .filter_map(|path| path.strip_prefix(repo_root).ok().map(Path::to_path_buf))
        .collect();
    configs.reverse();
    configs
}

/// The service's own directory, which is the repo root itself when the config
/// names none.
fn service_root(repo_root: &Path, directory: &Path) -> PathBuf {
    if directory == Path::new(".") {
        repo_root.to_path_buf()
    } else {
        repo_root.join(directory)
    }
}

/// Every directory from `from` up to and including `to`, when `from` is under
/// `to`. Empty when it is not, so a service directory that escapes the repo
/// asks about nothing.
fn ancestors_within(from: &Path, to: &Path) -> impl Iterator<Item = PathBuf> {
    let to = to.to_path_buf();
    from.ancestors()
        .take_while(move |dir| dir.starts_with(&to))
        .map(Path::to_path_buf)
        .collect::<Vec<_>>()
        .into_iter()
}

/// A repo-relative path for a directory inside the repo, so no absolute path
/// from this machine reaches the terminal or the cloud.
fn relative(dir: &Path, repo_root: &Path) -> String {
    dir.strip_prefix(repo_root)
        .unwrap_or(dir)
        .to_string_lossy()
        .to_string()
}

/// The repo root reads as `.` in a path and as "the repository root" in a
/// sentence.
fn display_dir(path: &str) -> String {
    if path.is_empty() || path == "." {
        "the repository root".to_string()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn service(name: &str, directory: Option<&str>) -> Config {
        Config {
            service_name: Some(name.to_string()),
            directory: directory.map(str::to_string),
            ..Config::default()
        }
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A nested workspace with its own lockfile is bare even though the repo
    /// root is installed — the measurement this check exists for.
    #[test]
    fn a_nested_workspace_is_bare_under_an_installed_root() {
        let repo = TempDir::new().unwrap();
        let root = repo.path();
        write(root, "package.json", r#"{"dependencies":{"express":"4"}}"#);
        write(root, "package-lock.json", "{}");
        fs::create_dir_all(root.join("node_modules")).unwrap();
        write(
            root,
            "apps/swan/package.json",
            r#"{"dependencies":{"fastify":"4"}}"#,
        );
        write(root, "apps/swan/pnpm-lock.yaml", "lockfileVersion: '9.0'");

        let found = unprepared(root, &[service("swan", Some("apps/swan"))]);
        assert_eq!(
            found,
            vec![Unprepared::Dependencies {
                service: "swan".to_string(),
                install_root: "apps/swan".to_string(),
                lockfile: "pnpm-lock.yaml",
                command: "pnpm install",
            }],
            "the root install is not this service's install"
        );
        let sentence = found[0].sentence();
        assert!(
            sentence.contains("`pnpm install` in apps/swan"),
            "the command names the directory it runs in: {sentence}"
        );
    }

    /// The same tree with its own `node_modules` is prepared, and a lockfile
    /// with nothing to install never refuses.
    #[test]
    fn an_installed_service_and_an_empty_manifest_are_prepared() {
        let repo = TempDir::new().unwrap();
        let root = repo.path();
        write(root, "package.json", r#"{"dependencies":{"express":"4"}}"#);
        write(root, "package-lock.json", "{}");
        fs::create_dir_all(root.join("node_modules")).unwrap();
        assert!(unprepared(root, &[service("api", None)]).is_empty());

        let bare = TempDir::new().unwrap();
        write(bare.path(), "package.json", r#"{"name":"nothing"}"#);
        write(bare.path(), "package-lock.json", "{}");
        assert!(
            unprepared(bare.path(), &[service("api", None)]).is_empty(),
            "a manifest that declares no dependency has no install to be missing"
        );
    }

    /// A tsconfig mapping to a directory the generator never filled.
    #[test]
    fn a_mapping_to_a_directory_that_is_not_there_is_refused() {
        let repo = TempDir::new().unwrap();
        let root = repo.path();
        write(
            root,
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{"@client/*":["./src/generated/client/*"]}}}"#,
        );
        write(root, "src/app.ts", "export const a = 1;");

        let found = unprepared(root, &[service("api", None)]);
        assert_eq!(
            found,
            vec![Unprepared::Mapping {
                service: "api".to_string(),
                declared_by: "@client/*".to_string(),
                target: "src/generated/client".to_string(),
            }]
        );
        let sentence = found[0].sentence();
        assert!(
            sentence.contains("maps `@client/*` to src/generated/client"),
            "the mapping and the missing path are both named: {sentence}"
        );
        assert!(
            !sentence.contains("Run `"),
            "nothing in config says what fills it, so no command is invented: {sentence}"
        );

        fs::create_dir_all(root.join("src/generated/client")).unwrap();
        assert!(
            unprepared(root, &[service("api", None)]).is_empty(),
            "the directory existing is the whole claim"
        );
    }

    /// The override turns both refusals into a warning, which is what a CI job
    /// that checks out without installing passes.
    #[test]
    #[serial_test::serial(allow_unprepared)]
    fn the_override_lets_an_unprepared_tree_through() {
        let repo = TempDir::new().unwrap();
        let root = repo.path();
        write(root, "package.json", r#"{"dependencies":{"express":"4"}}"#);
        write(root, "package-lock.json", "{}");

        let found = unprepared(root, &[service("api", None)]);
        assert_eq!(found.len(), 1);
        let message = refusal(found.clone()).expect("refused without the override");
        assert!(message.contains(ALLOW_FLAG), "{message}");

        // SAFETY: the test is serialised on the same key as every other test
        // that reads this variable, so no other thread is reading the
        // environment while it is set.
        unsafe { std::env::set_var(ALLOW_ENV, "1") };
        let allowed = refusal(found.clone());
        unsafe { std::env::remove_var(ALLOW_ENV) };
        assert!(
            allowed.is_none(),
            "the override is the way a deliberate bare scan proceeds"
        );
    }

    /// A Deno service caches outside the tree unless its config asks for a
    /// local `node_modules`.
    #[test]
    fn a_deno_service_is_asked_only_when_its_config_wants_node_modules() {
        let repo = TempDir::new().unwrap();
        let root = repo.path();
        write(root, "deno.json", r#"{"imports":{"zod":"npm:zod@3"}}"#);
        write(root, "deno.lock", "{}");
        assert!(
            unprepared(root, &[service("api", None)]).is_empty(),
            "an absent node_modules says nothing about a Deno cache"
        );

        write(
            root,
            "deno.json",
            r#"{"nodeModulesDir":"auto","imports":{"zod":"npm:zod@3"}}"#,
        );
        assert_eq!(
            unprepared(root, &[service("api", None)]),
            vec![Unprepared::Dependencies {
                service: "api".to_string(),
                install_root: String::new(),
                lockfile: "deno.lock",
                command: "deno install",
            }],
            "a config that asks for a local node_modules is asked about it"
        );
    }
}
