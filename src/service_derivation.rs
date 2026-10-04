//! Service selection shared by CI, local indexing and the npm init preview.
//! Explicit configuration wins; inferred package boundaries remain editable.
//!
//! Three things state a service boundary, read in this order:
//!
//! 1. `carrick.json`, which is taken as written.
//! 2. A declared workspace: npm or pnpm patterns, or a Deno manifest at the
//!    root. The declaration alone decides; a lockfile inside a member, or
//!    beside a package no pattern claims, changes nothing.
//! 3. With neither, a manifest with a lockfile beside it, below the root: a
//!    package that installs on its own ([`lockfile_rooted_packages`]),
//!    proposed beside the root where the root holds source of its own.
//!
//! A repository that states none of them is one service at its root.
//!
//! Services may sit inside each other in all three: a file belongs to the
//! deepest service whose directory holds it (`Config::resolve_nested`,
//! carrick#553).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::packages::{
    MANIFEST_SKIP_DIRS, ManifestFacts, deno_workspace_manifest_paths, holds_lockfile, read_manifest,
};

#[derive(Debug, Serialize)]
pub struct ServiceDerivation {
    pub reason: String,
    pub services: Vec<Config>,
    /// One entry per service, in the same order, carrying what the member's
    /// own manifest says about it (carrick#994).
    pub members: Vec<MemberFacts>,
    /// The exact proposal init may create with exclusive-create semantics.
    pub config: serde_json::Value,
    /// What THIS derivation found that the reader has to act on. Printed by
    /// `carrick init`, one line each, so anything here has to be a fact about
    /// this workspace rather than advice that holds for every workspace.
    pub warnings: Vec<String>,
    /// Standing advice about a proposal of this shape: true of every Deno
    /// repo, or every workspace with more than one member, whatever this run
    /// found. It rides in `.carrick/proposal.json` and the quickstart, and the
    /// terminal never prints it — printed, it was a warning marker on a first
    /// run that had raised no warning at all (carrick#1032).
    pub notes: Vec<String>,
}

/// The manifest facts that decide application from library.
///
/// Every workspace member is derived as a service here, and the whole
/// application-versus-library decision then sat in the scaffold instructions,
/// so the agent had to re-derive it by walking imports (carrick#994). These
/// are the facts that decision is actually made on, read from what the member
/// declares about itself and from what its siblings declare about it.
///
/// Every field is a statement, never a guess: `false` and `[]` mean the
/// manifest declares nothing, not that nothing is there. Field names are
/// snake_case, unlike the `carrick.json` keys they sit beside, because these
/// are derived facts rather than configuration anyone writes.
#[derive(Debug, Serialize, Clone, Default, PartialEq, Eq)]
pub struct MemberFacts {
    /// `private: true`: this package is not published.
    pub private: bool,
    /// A declared `bin`, which is an executable rather than an import target.
    pub bin: bool,
    /// A declared `main`.
    pub main: bool,
    /// A declared `exports`, the modern statement of an import surface.
    ///
    /// It separates a library from an application in an npm workspace and it
    /// does NOT in a Deno one: a Deno workspace member declares `name` and
    /// `exports` to be importable by its siblings at all, so every member of a
    /// Deno workspace carries it, applications included (carrick#1007 item 6).
    /// `workspace_dependents` is the field that separates them there.
    pub exports: bool,
    /// Deployment descriptors in the member's own directory, in a fixed order.
    pub deploy_config: Vec<String>,
    /// The other members that depend on this one, by the name this proposal
    /// gives them.
    ///
    /// From the manifests where the manifests record it (an npm dependency
    /// entry, a Deno import-map path), and from the import specifiers in a
    /// Deno member's own source where they do not: a Deno member is imported
    /// by its workspace NAME and no manifest anywhere records that edge.
    pub workspace_dependents: Vec<String>,
}

impl ServiceDerivation {
    /// The `services` array of `carrick.derive/0`: each service's
    /// configuration and the facts about the member it came from, in one
    /// object per service.
    ///
    /// Additive, so the schema tag does not move. The facts are deliberately
    /// absent from `config`, which is the `carrick.json` skeleton someone
    /// writes: `private` and `bin` are things a repository states about
    /// itself, never configuration Carrick accepts.
    pub fn service_documents(&self) -> Vec<serde_json::Value> {
        #[derive(Serialize)]
        struct ServiceDocument<'a> {
            #[serde(flatten)]
            config: &'a Config,
            #[serde(flatten)]
            facts: &'a MemberFacts,
        }
        self.services
            .iter()
            .zip(self.members.iter())
            .map(|(config, facts)| {
                serde_json::to_value(ServiceDocument { config, facts })
                    .expect("a service and its facts both serialize as JSON objects")
            })
            .collect()
    }
}

/// Deployment descriptors, read only in the member's own directory: one of
/// these beside a package is the strongest statement in a repository that it
/// is deployed rather than imported.
const DEPLOY_CONFIG: [&str; 5] = [
    "netlify.toml",
    "vercel.json",
    "serverless.yml",
    "wrangler.toml",
    "Dockerfile",
];

#[derive(Deserialize)]
struct PnpmWorkspace {
    #[serde(default)]
    packages: Vec<String>,
}

/// Read the package-manager declarations without inventing a Node manifest.
pub fn workspace_patterns(root: &Path) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut sources = Vec::new();
    let package = root.join("package.json");
    if package.try_exists().map_err(|e| e.to_string())? {
        let text = std::fs::read_to_string(&package).map_err(|e| e.to_string())?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("Failed to parse {}: {e}", package.display()))?;
        if let Some(workspaces) = value.get("workspaces") {
            let patterns = if workspaces.is_object() {
                workspaces.get("packages").ok_or_else(|| {
                    format!("{}: workspaces needs a packages array", package.display())
                })?
            } else {
                workspaces
            };
            let patterns: Vec<String> = serde_json::from_value(patterns.clone())
                .map_err(|e| format!("{}: invalid workspaces: {e}", package.display()))?;
            sources.push(("npm workspaces".to_string(), patterns));
        }
    }
    let pnpm = root.join("pnpm-workspace.yaml");
    if pnpm.try_exists().map_err(|e| e.to_string())? {
        let text = std::fs::read_to_string(&pnpm).map_err(|e| e.to_string())?;
        let workspace: PnpmWorkspace = serde_yaml_ng::from_str(&text)
            .map_err(|e| format!("Failed to parse {}: {e}", pnpm.display()))?;
        sources.push(("pnpm workspaces".to_string(), workspace.packages));
    }
    Ok(sources)
}

/// Resolve once, including the validation CI applies before scanning any files.
pub fn resolve(root: &Path) -> Result<ServiceDerivation, String> {
    let config_path = root.join("carrick.json");
    // symlink_metadata also sees broken symlinks: an existing path is never
    // mistaken for permission to infer and replace an explicit declaration.
    match std::fs::symlink_metadata(&config_path) {
        Ok(_) => {
            let services = Config::load_services(vec![config_path.clone()]).map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidInput {
                    e.to_string()
                } else {
                    format!("Failed to parse {}: {e}", config_path.display())
                }
            })?;
            validate(root, &services)?;
            let members = member_facts(root, &services);
            return Ok(ServiceDerivation {
                reason: "carrick.json".into(),
                config: serde_json::Value::Null,
                members,
                services,
                warnings: Vec::new(),
                notes: Vec::new(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Could not read {}: {e}", config_path.display())),
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let sources = workspace_patterns(&root)?;
    let mut members: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let mut reasons = sources
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    if !sources.is_empty() {
        let options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: true,
        };
        let mut positive = Vec::new();
        let mut negative = Vec::new();
        for (_, patterns) in &sources {
            for raw in patterns {
                let pattern = raw
                    .strip_prefix('!')
                    .unwrap_or(raw)
                    .trim_start_matches("./")
                    .trim_end_matches('/');
                if pattern.is_empty()
                    || Path::new(pattern).is_absolute()
                    || pattern.contains(['{', '}', '\\'])
                    || Path::new(pattern)
                        .components()
                        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
                {
                    return Err(format!(
                        "Unsupported workspace pattern '{raw}'; use repository-relative glob paths or an explicit carrick.json."
                    ));
                }
                let compiled = glob::Pattern::new(pattern)
                    .map_err(|e| format!("Invalid workspace pattern '{raw}': {e}"))?;
                if raw.starts_with('!') {
                    negative.push(compiled);
                } else {
                    positive.push((raw, compiled, false));
                }
            }
        }
        for entry in manifest_walk(&root) {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().is_file() || entry.file_name() != "package.json" {
                continue;
            }
            let directory = entry.path().parent().expect("manifest parent");
            let relative = directory.strip_prefix(&root).map_err(|e| e.to_string())?;
            let relative = if relative.as_os_str().is_empty() {
                Path::new(".")
            } else {
                relative
            };
            let mut matched = false;
            for (_, pattern, seen) in &mut positive {
                if pattern.matches_path_with(relative, options) {
                    *seen = true;
                    matched = true;
                }
            }
            if matched
                && !negative
                    .iter()
                    .any(|p| p.matches_path_with(relative, options))
            {
                members.insert(directory.to_path_buf(), entry.path().to_path_buf());
            }
        }
        for (raw, _, seen) in positive {
            if !seen {
                return Err(format!(
                    "Workspace pattern '{raw}' matched no package manifests; correct it or declare services in carrick.json."
                ));
            }
        }
    }
    let deno = deno_workspace_manifest_paths(&root).map_err(|e| e.to_string())?;
    let mut has_deno = !deno.is_empty();
    if has_deno {
        reasons.push("Deno manifests".into());
        for manifest in deno {
            let directory = manifest
                .parent()
                .expect("manifest parent")
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let facts = read_manifest(&manifest).map_err(|e| e.to_string())?;
            // A workspace-only root does not constitute an extra service.
            if directory == root && !facts.workspace.is_empty() {
                continue;
            }
            members.entry(directory).or_insert(manifest);
        }
    }
    // What this derivation found that the reader has to act on, and the
    // package names more than one lockfile-rooted package declares.
    let mut warnings = Vec::new();
    let mut shared_names = BTreeSet::new();
    let mut lockfile_rooted = false;
    if members.is_empty() {
        if !sources.is_empty() && sources.iter().any(|(_, patterns)| !patterns.is_empty()) {
            return Err("Workspace patterns select no services; declare the intended services in carrick.json.".into());
        }
        // Asked only of a repository that declares no workspace at all: a
        // declaration is the repository's own statement of its packages, even
        // one that lists none.
        let found = if sources.is_empty() {
            lockfile_rooted_packages(&root)?
        } else {
            None
        };
        match found {
            Some(found) => {
                reasons.push("lockfile-rooted packages".into());
                warnings.extend(found.unlocked_warning());
                has_deno = found
                    .members
                    .values()
                    .any(|manifest| manifest.file_name().is_some_and(|n| n != "package.json"));
                members = found.members;
                shared_names = found.shared_names;
                lockfile_rooted = true;
            }
            None => {
                members.insert(root.clone(), root.join("package.json"));
            }
        }
    }
    let mut names = BTreeSet::new();
    let mut services = Vec::new();
    for (directory, manifest) in members {
        let relative = directory.strip_prefix(&root).map_err(|e| e.to_string())?;
        let facts = if manifest.is_file() {
            Some(read_manifest(&manifest).map_err(|e| e.to_string())?)
        } else {
            None
        };
        // A plain root service is identified by its repo, as before. Package
        // names identify workspace members without changing root cache keys.
        // Lockfile-rooted packages that share a package name take their
        // directory: nothing claims they are one workspace, so nothing made
        // their names unique, and the one service this repository used to be
        // never failed over it.
        let name = facts
            .and_then(|facts| facts.package.name)
            .filter(|name| !relative.as_os_str().is_empty() && !shared_names.contains(name))
            .or_else(|| {
                (!relative.as_os_str().is_empty())
                    .then(|| relative.to_string_lossy().replace('\\', "/"))
            });
        if let Some(name) = &name
            && !names.insert(name.clone())
        {
            return Err(format!(
                "Derived service name '{name}' is duplicated; declare unique names in carrick.json."
            ));
        }
        let tsconfig = if crate::deno_support::service_manifest(
            &root,
            &Config {
                directory: Some(relative.to_string_lossy().into_owned()),
                ..Config::default()
            },
        )
        .is_some()
        {
            None
        } else {
            nearest_tsconfig(&root, &directory)
        };
        services.push(Config {
            service_name: name,
            directory: (!relative.as_os_str().is_empty())
                .then(|| relative.to_string_lossy().replace('\\', "/")),
            tsconfig,
            ..Config::default()
        });
    }
    // A derived member inside another member is that member's own source, as
    // it is for a declared list (carrick#553).
    Config::resolve_nested(&mut services);
    validate(&root, &services)?;
    let config = serde_json::json!({ "services": services });
    // These are true of every proposal of this shape and none is something
    // this run found, so they are notes rather than warnings: they belong to
    // the document the agent reads and to the quickstart, not to a terminal
    // line with a warning marker on it (carrick#1032).
    let mut notes = Vec::new();
    if lockfile_rooted {
        notes.push("Directories holding their own manifest and lockfile are proposed as services. Review service boundaries and shared source includes in carrick.json.".into());
    } else if services.len() > 1 {
        notes.push("Workspace packages are proposed as services. Review service boundaries and shared source includes in carrick.json.".into());
    }
    if has_deno {
        notes.push("Deno services use their existing manifests and require Deno on PATH for type resolution.".into());
    }
    let members = member_facts(&root, &services);
    Ok(ServiceDerivation {
        reason: if reasons.is_empty() {
            "single repository".into()
        } else {
            reasons.join(" + ")
        },
        members,
        services,
        config,
        warnings,
        notes,
    })
}

/// The walk every manifest search below a repository root makes: sorted, and
/// never into a dot directory, a dependency install or build output.
fn manifest_walk(root: &Path) -> impl Iterator<Item = walkdir::Result<walkdir::DirEntry>> {
    walkdir::WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || entry.file_name().to_str().is_some_and(|name| {
                    !name.starts_with('.')
                        && !MANIFEST_SKIP_DIRS.contains(&name)
                        && !["target", "out", "coverage"].contains(&name)
                })
        })
}

/// What [`lockfile_rooted_packages`] proposes.
struct LockfileRooted {
    /// Directory to manifest, as the declared-workspace branches fill it. The
    /// root is among them when it holds source of its own.
    members: BTreeMap<PathBuf, PathBuf>,
    /// Package names more than one of those manifests declares.
    shared_names: BTreeSet<String>,
    /// Directories, relative to the root, whose manifest declares
    /// dependencies with no lockfile beside it: packages that are not
    /// proposed and are indexed with the root.
    unlocked: Vec<PathBuf>,
}

impl LockfileRooted {
    /// How many such directories are named before the rest are counted.
    const PLACES_SHOWN: usize = 3;

    /// The line for packages that declare dependencies and state no install,
    /// or `None` when there are none. The lockfile is what states that a
    /// package installs on its own, so one that is not committed leaves its
    /// package unproposed; this says so, since nothing else would.
    fn unlocked_warning(&self) -> Option<String> {
        if self.unlocked.is_empty() {
            return None;
        }
        let mut named = self
            .unlocked
            .iter()
            .take(Self::PLACES_SHOWN)
            .map(|directory| format!("`{}`", directory.to_string_lossy().replace('\\', "/")))
            .collect::<Vec<_>>()
            .join(", ");
        if self.unlocked.len() > Self::PLACES_SHOWN {
            named.push_str(&format!(
                " and {} more",
                self.unlocked.len() - Self::PLACES_SHOWN
            ));
        }
        Some(format!(
            "{named}: a manifest that declares dependencies with no lockfile beside it, so not proposed as a service and indexed with the repository root. An uncommitted lockfile is the usual cause: commit it, or declare the service in carrick.json."
        ))
    }
}

/// Whether a manifest declares anything to install. A manifest that declares
/// nothing (a module-type marker, say) is not a package missing its lockfile.
fn declares_an_install(facts: &ManifestFacts) -> bool {
    let package = &facts.package;
    !package.dependencies.is_empty()
        || !package.dev_dependencies.is_empty()
        || !package.peer_dependencies.is_empty()
        || !package.optional_dependencies.is_empty()
}

/// The packages below `root` that install on their own, in a repository that
/// declares no workspace (carrick#1854).
///
/// A manifest with a lockfile beside it states an install, and a repository
/// holding several under a root that claims none of them is as many
/// applications as it has installs: the usual shape of a client and the
/// server it calls kept in one repository. Proposed as one service, every
/// call between them is a call a service makes to itself.
///
/// What is read, and nothing else:
///
/// * **A lockfile beside a manifest**, for every package manager the scanner
///   recognises ([`holds_lockfile`]). A manifest alone states no install: it
///   is as often a module-type marker or a fixture as a package. One that
///   declares dependencies is named in a warning
///   ([`LockfileRooted::unlocked_warning`]).
/// * **Source of its own that a scan of this repository reads.** A file
///   belongs to the deepest service whose directory holds it (carrick#553),
///   so a package is proposed for the files under it that no package beneath
///   it holds. A directory with none (an install of tooling, a suite under a
///   test directory, a folder that only holds other packages) would be a
///   service with nothing in it, which a scan refuses.
///
/// The root is proposed beside them on the same terms: it is a service when
/// it holds source no package below it holds. So every file the one service
/// read is still read, once, and packages nested in each other are each
/// proposed.
///
/// `None` when no package is proposed, which leaves the repository one
/// service. A directory or a nested manifest that cannot be read is passed
/// over rather than failed on: the repository was one service before this
/// looked.
fn lockfile_rooted_packages(root: &Path) -> Result<Option<LockfileRooted>, String> {
    // Directory to its manifest and the package name that manifest declares.
    let mut candidates: BTreeMap<PathBuf, (PathBuf, Option<String>)> = BTreeMap::new();
    let mut unlocked: Vec<PathBuf> = Vec::new();
    for entry in manifest_walk(root).flatten() {
        if entry.depth() == 0 || !entry.file_type().is_dir() {
            continue;
        }
        let Some(manifest) = manifest_in(entry.path()) else {
            continue;
        };
        let Ok(facts) = read_manifest(&manifest) else {
            continue;
        };
        if holds_lockfile(entry.path()) {
            candidates.insert(entry.path().to_path_buf(), (manifest, facts.package.name));
        } else if declares_an_install(&facts) {
            unlocked.push(entry.path().to_path_buf());
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    // The files the one service this repository would otherwise be reads, so
    // "holds source" means the same thing here as it does to a scan.
    let (source, _) = crate::file_finder::find_files(&root.to_string_lossy(), &MANIFEST_SKIP_DIRS);
    candidates.retain(|directory, _| source.iter().any(|file| file.starts_with(directory)));

    // Whether a file under `directory` is in none of the packages beneath it.
    let holds_own_source = |directory: &Path| {
        source.iter().any(|file| {
            file.starts_with(directory)
                && !candidates.keys().any(|inner| {
                    inner != directory && inner.starts_with(directory) && file.starts_with(inner)
                })
        })
    };
    let mut members: BTreeMap<PathBuf, PathBuf> = candidates
        .iter()
        .filter(|(directory, _)| holds_own_source(directory))
        .map(|(directory, (manifest, _))| (directory.clone(), manifest.clone()))
        .collect();
    if members.is_empty() {
        return Ok(None);
    }
    // A package with no lockfile is indexed with the root only where no
    // lockfile-rooted package holds it: inside one it may be that package's
    // own workspace member, installed by the lockfile above it.
    unlocked.retain(|directory| {
        !candidates
            .keys()
            .any(|package| directory.starts_with(package))
            && source.iter().any(|file| file.starts_with(directory))
    });
    let unlocked = unlocked
        .iter()
        .filter_map(|directory| directory.strip_prefix(root).ok().map(Path::to_path_buf))
        .collect();
    // A package name names a service only where it can name one alone: a name
    // two of these declare, or one that is another's directory, gives way to
    // the directory each sits in, and directories are distinct.
    let mut seen_names: BTreeSet<String> = members
        .keys()
        .filter_map(|directory| directory.strip_prefix(root).ok())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .collect();
    let mut shared_names = BTreeSet::new();
    for directory in members.keys() {
        let relative = directory.strip_prefix(root).unwrap_or(directory);
        if let Some(name) = &candidates[directory].1
            && name.as_str() != relative.to_string_lossy().replace('\\', "/")
            && !seen_names.insert(name.clone())
        {
            shared_names.insert(name.clone());
        }
    }
    // The root, for what is left: the files no proposed package holds. It
    // keeps the manifest path a single-service repository reads, present or
    // not, and stays unnamed, so it is the service the repository already was.
    if source
        .iter()
        .any(|file| !members.keys().any(|directory| file.starts_with(directory)))
    {
        members.insert(root.to_path_buf(), root.join("package.json"));
    }
    Ok(Some(LockfileRooted {
        members,
        shared_names,
        unlocked,
    }))
}

/// The manifest a directory holds, npm's before Deno's.
fn manifest_in(directory: &Path) -> Option<PathBuf> {
    let package = directory.join("package.json");
    if package.is_file() {
        return Some(package);
    }
    crate::deno_support::manifest_at(directory)
}

/// The manifest a service's own directory holds.
fn member_manifest(root: &Path, service: &Config) -> Option<PathBuf> {
    manifest_in(&root.join(service.directory.as_deref().unwrap_or(".")))
}

/// What this proposal calls a service, so a dependent names it the same way.
fn label(service: &Config) -> String {
    service
        .service_name
        .clone()
        .or_else(|| service.directory.clone())
        .unwrap_or_else(|| ".".to_string())
}

/// What every member declares about itself, and what its siblings declare
/// about it (carrick#994).
///
/// Manifests only. `derive` runs inside `carrick init`, before anything has
/// been scanned, so a dependency edge here is one a package manager already
/// records: a dependency entry naming another member's package, or a Deno
/// import-map target resolving inside another member's directory. Walking
/// imports would answer more and would make a first run pay for a parse it
/// has nowhere to put.
///
/// Returns one entry per service, in the same order.
pub fn member_facts(root: &Path, services: &[Config]) -> Vec<MemberFacts> {
    let manifests: Vec<Option<crate::packages::ManifestFacts>> = services
        .iter()
        .map(|service| member_manifest(root, service).and_then(|path| read_manifest(&path).ok()))
        .collect();
    let directories: Vec<PathBuf> = services
        .iter()
        .map(|service| {
            crate::workspace_resolver::normalize(
                &root.join(service.directory.as_deref().unwrap_or(".")),
            )
        })
        .collect();
    let mut by_package: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, manifest) in manifests.iter().enumerate() {
        if let Some(name) = manifest
            .as_ref()
            .and_then(|facts| facts.package.name.as_deref())
        {
            by_package.entry(name).or_insert(index);
        }
    }

    let mut facts: Vec<MemberFacts> = services
        .iter()
        .enumerate()
        .map(|(index, service)| {
            let manifest = manifests[index].as_ref();
            let directory = root.join(service.directory.as_deref().unwrap_or("."));
            MemberFacts {
                private: manifest.is_some_and(|facts| facts.private),
                bin: manifest.is_some_and(|facts| facts.bin),
                main: manifest.is_some_and(|facts| facts.main.is_some()),
                exports: manifest.is_some_and(|facts| facts.exports.is_some()),
                deploy_config: DEPLOY_CONFIG
                    .iter()
                    .filter(|name| directory.join(name).is_file())
                    .map(|name| (*name).to_string())
                    .collect(),
                workspace_dependents: Vec::new(),
            }
        })
        .collect();

    for (index, manifest) in manifests.iter().enumerate() {
        let Some(manifest) = manifest else { continue };
        let mut depends_on: BTreeSet<usize> = BTreeSet::new();
        let package = &manifest.package;
        for name in package
            .dependencies
            .keys()
            .chain(package.dev_dependencies.keys())
            .chain(package.peer_dependencies.keys())
            .chain(package.optional_dependencies.keys())
        {
            if let Some(&other) = by_package.get(name.as_str()) {
                depends_on.insert(other);
            }
        }
        for target in &manifest.local_imports {
            let resolved =
                crate::workspace_resolver::normalize(&directories[index].join(target.as_str()));
            // The deepest member the target sits in, so a package inside
            // another package's directory takes its own import.
            if let Some((other, _)) = directories
                .iter()
                .enumerate()
                .filter(|(_, directory)| resolved.starts_with(directory))
                .max_by_key(|(_, directory)| directory.as_os_str().len())
            {
                depends_on.insert(other);
            }
        }
        depends_on.remove(&index);
        for other in depends_on {
            facts[other]
                .workspace_dependents
                .push(label(&services[index]));
        }
    }
    // Where no manifest can record the edge, the source is the record. A Deno
    // workspace member is importable by the `name` it declares, and a sibling
    // that imports it writes that name and nothing else: no dependency entry,
    // no import-map path, nothing in the lock file (which records a member's
    // dependencies only when the member declares some). Reading the specifiers
    // is the only way not to report `[]` — a claim — for every member of every
    // Deno workspace (carrick#1007 item 6, following carrick#994).
    for (index, manifest) in manifests.iter().enumerate() {
        if manifest.is_none() || !is_deno_member(root, &services[index]) {
            continue;
        }
        for other in imported_members(&directories, index, &by_package, &manifests) {
            facts[other]
                .workspace_dependents
                .push(label(&services[index]));
        }
    }
    for entry in &mut facts {
        entry.workspace_dependents.sort();
        entry.workspace_dependents.dedup();
    }
    facts
}

/// Whether this service's own manifest is a Deno config rather than a
/// `package.json`. Only there is a sibling dependency absent from every
/// manifest.
fn is_deno_member(root: &Path, service: &Config) -> bool {
    member_manifest(root, service)
        .is_some_and(|path| path.file_name().is_some_and(|name| name != "package.json"))
}

/// The members whose declared package name this member's source imports.
///
/// Bounded on purpose: the walk skips what every scan skips, a file is read
/// as text and parsed only when it holds a member's name at all, and a file
/// that belongs to a deeper member is that member's, not this one's. A file
/// that does not parse contributes nothing, like every other read here.
fn imported_members(
    directories: &[PathBuf],
    index: usize,
    by_package: &BTreeMap<&str, usize>,
    manifests: &[Option<crate::packages::ManifestFacts>],
) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    if by_package.is_empty() {
        return found;
    }
    let (files, _) =
        crate::file_finder::find_files(&directories[index].to_string_lossy(), &MANIFEST_SKIP_DIRS);
    let mut resolver = crate::parser::ModuleReader::default();
    for file in files {
        // A file inside a nested member is that member's source, and its
        // imports are that member's dependencies.
        let owner = directories
            .iter()
            .enumerate()
            .filter(|(_, directory)| file.starts_with(directory))
            .max_by_key(|(_, directory)| directory.as_os_str().len())
            .map(|(owner, _)| owner);
        if owner != Some(index) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        // Nothing can be imported from a file that does not name it, so the
        // parse is only paid for where an edge is possible.
        if !by_package.keys().any(|name| text.contains(name)) {
            continue;
        }
        for specifier in resolver.import_specifiers(&file) {
            let Some(&other) = by_package.get(specifier.as_str()).or_else(|| {
                // `@scope/shared/sub` imports the member `@scope/shared`.
                by_package
                    .iter()
                    .find(|(name, _)| {
                        specifier.starts_with(*name)
                            && specifier.as_bytes().get(name.len()) == Some(&b'/')
                    })
                    .map(|(_, other)| other)
            }) else {
                continue;
            };
            if other != index && manifests[other].is_some() {
                found.insert(other);
            }
        }
    }
    found.remove(&index);
    found
}

fn nearest_tsconfig(root: &Path, directory: &Path) -> Option<String> {
    let mut prefix = PathBuf::new();
    for ancestor in directory.ancestors() {
        if !ancestor.starts_with(root) {
            break;
        }
        if ancestor.join("tsconfig.json").is_file() {
            return Some(
                prefix
                    .join("tsconfig.json")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
        prefix.push("..");
    }
    None
}

fn validate(root: &Path, services: &[Config]) -> Result<(), String> {
    for service in services {
        let label = service
            .service_name
            .as_deref()
            .or(service.directory.as_deref())
            .unwrap_or("<unnamed>");
        if let Some(directory) = &service.directory
            && !root.join(directory).is_dir()
        {
            return Err(format!(
                "Service '{label}' in {} declares directory '{directory}', which does not exist under '{}'",
                root.join("carrick.json").display(),
                root.display()
            ));
        }
        for include in &service.include {
            if !root.join(include).exists() {
                return Err(format!(
                    "Service '{label}' declares include path '{include}', which does not exist under '{}'",
                    root.display()
                ));
            }
        }
        if let Some(tsconfig) = &service.tsconfig {
            let path = root
                .join(service.directory.as_deref().unwrap_or("."))
                .join(tsconfig);
            if !path.is_file() {
                return Err(format!(
                    "Service '{label}' declares tsconfig '{}', which does not exist",
                    path.display()
                ));
            }
            if path
                .file_name()
                .is_some_and(|name| name == "deno.json" || name == "deno.jsonc")
            {
                let directory = root.join(service.directory.as_deref().unwrap_or("."));
                let nearest = directory
                    .ancestors()
                    .take_while(|p| p.starts_with(root))
                    .find_map(crate::deno_support::manifest_at);
                let selected = path.canonicalize().map_err(|e| e.to_string())?;
                if !nearest.is_some_and(|nearest| {
                    nearest.file_name() == path.file_name()
                        && nearest.canonicalize().ok().as_ref() == Some(&selected)
                }) {
                    return Err(format!(
                        "Service '{label}' must select its nearest Deno manifest (deno.json takes precedence over deno.jsonc). Alternate Deno config '{}' is not supported; omit tsconfig to use the service manifest, or select an ordinary TypeScript config.",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}
