//! Service selection shared by CI, local indexing and the npm init preview.
//! Explicit configuration wins; inferred package boundaries remain editable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::packages::{MANIFEST_SKIP_DIRS, deno_workspace_manifest_paths, read_manifest};

#[derive(Debug, Serialize)]
pub struct ServiceDerivation {
    pub reason: String,
    pub services: Vec<Config>,
    /// One entry per service, in the same order, carrying what the member's
    /// own manifest says about it (carrick#994).
    pub members: Vec<MemberFacts>,
    /// The exact proposal init may create with exclusive-create semantics.
    pub config: serde_json::Value,
    /// What THIS derivation found that the reader has to act on, one line
    /// each, so anything here has to be a fact about this workspace rather
    /// than advice that holds for every workspace.
    pub warnings: Vec<String>,
    /// Standing advice about a proposal of this shape: true of every Deno
    /// repo, or every workspace with more than one member, whatever this run
    /// found. It rides in the derive document and the quickstart, and the
    /// terminal never prints it — printed, it was a warning marker on a first
    /// run that had raised no warning at all (carrick#1032).
    pub notes: Vec<String>,
    /// Workspace members that are no service because they hold no source
    /// file, as repo-relative directories (carrick#2298). A scan names them in
    /// its closing lines; the derive document carries them in `notes`.
    #[serde(skip)]
    pub without_source: Vec<String>,
    /// Workspace patterns that matched no package and were skipped
    /// (carrick#3318). The derive document carries them as `skipped_patterns`.
    #[serde(skip)]
    pub skipped_patterns: Vec<SkippedPattern>,
}

/// A workspace pattern derive left out, and why.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct SkippedPattern {
    pub pattern: String,
    pub reason: String,
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
pub const DEPLOY_CONFIG: [&str; 5] = [
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

/// The workspace globs [`workspace_patterns`] reads, compiled: which
/// directories, relative to the root that declares them, are members.
pub struct WorkspaceGlobs {
    /// Each positive pattern as written, with its compiled form.
    positive: Vec<(String, glob::Pattern)>,
    /// The `!` patterns, which leave a matched directory out.
    negative: Vec<glob::Pattern>,
}

const WORKSPACE_MATCH: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: true,
};

impl WorkspaceGlobs {
    /// `Err` on a pattern this rule does not read: absolute, outside the
    /// root, or using braces or backslashes.
    pub fn compile(sources: &[(String, Vec<String>)]) -> Result<Self, String> {
        let mut globs = WorkspaceGlobs {
            positive: Vec::new(),
            negative: Vec::new(),
        };
        for (_, patterns) in sources {
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
                    globs.negative.push(compiled);
                } else {
                    globs.positive.push((raw.clone(), compiled));
                }
            }
        }
        Ok(globs)
    }

    /// The index of each positive pattern that matches `relative`.
    pub fn matching<'a>(&'a self, relative: &'a Path) -> impl Iterator<Item = usize> + 'a {
        self.positive
            .iter()
            .enumerate()
            .filter(move |(_, (_, pattern))| pattern.matches_path_with(relative, WORKSPACE_MATCH))
            .map(|(index, _)| index)
    }

    /// Whether a `!` pattern leaves `relative` out.
    pub fn excludes(&self, relative: &Path) -> bool {
        self.negative
            .iter()
            .any(|pattern| pattern.matches_path_with(relative, WORKSPACE_MATCH))
    }

    /// Whether the directory `relative` is a workspace member.
    pub fn holds(&self, relative: &Path) -> bool {
        self.matching(relative).next().is_some() && !self.excludes(relative)
    }
}

/// The services a scan of `root` reads (carrick#2221): the ones in the tree's
/// `carrick.json`, or, for a no-model re-read of a repo without one, the
/// configs its hosted index was written with.
///
/// The preflight (`main.rs`) and the engine both call this, so they see one
/// list. Only a no-model pass takes the hosted configs: CI, the runner and
/// every scan that asks the model resolve from the tree as before.
pub fn resolve_for_scan(root: &Path) -> Result<Vec<Config>, String> {
    scan_services(root).map(|scan| scan.services)
}

/// [`resolve_for_scan`]'s list, with what the engine needs to know about
/// where it came from (carrick#2298).
#[derive(Debug)]
pub struct ScanServices {
    pub services: Vec<Config>,
    /// The list is the tree's own `carrick.json`: somebody named each
    /// service, so one that holds no source is a wrong path and fails the
    /// scan. A list nobody wrote (derived from manifests, or the hosted
    /// index's) leaves such a service out and says so.
    pub declared: bool,
    /// Workspace members the derivation left out because they hold no source
    /// file, as repo-relative directories.
    pub without_source: Vec<String>,
}

/// The services a scan of `root` reads and where the list came from.
pub fn scan_services(root: &Path) -> Result<ScanServices, String> {
    resolve_gated(root, crate::local_mode::no_model(), || {
        crate::local_mode::hosted::previous_data()
            .map_err(|e| tracing::debug!("No hosted previous generation for services: {e}"))
            .ok()
            .flatten()
    })
}

/// [`scan_services`] with its two inputs from the environment handed in.
fn resolve_gated(
    root: &Path,
    no_model: bool,
    previous: impl FnOnce() -> Option<Vec<crate::cloud_storage::CloudRepoData>>,
) -> Result<ScanServices, String> {
    // A tree that holds its own config never reads the previous generation.
    let hosted = if no_model && !has_root_config(root)? {
        previous()
    } else {
        None
    };
    let configs = hosted.as_deref().map(config_jsons);
    scan_services_with_hosted(root, configs.as_deref())
}

/// The `config_json` of each blob, in order, for [`scan_services_with_hosted`].
fn config_jsons(blobs: &[crate::cloud_storage::CloudRepoData]) -> Vec<Option<&str>> {
    blobs
        .iter()
        .map(|blob| blob.config_json.as_deref())
        .collect()
}

/// [`resolve_for_scan`] with the hosted blobs handed in, for a caller that
/// holds them itself (the indexer's progress count) and for tests.
///
/// The hosted configs are taken all or nothing: every blob must carry a
/// `config_json` that parses and names directories this tree holds, or the
/// list is derived exactly as without them. Half a hosted split and half a
/// derived one is a split neither side wrote.
pub fn resolve_with_hosted(
    root: &Path,
    hosted: Option<&[crate::cloud_storage::CloudRepoData]>,
) -> Result<Vec<Config>, String> {
    let configs = hosted.map(config_jsons);
    resolve_with_configs(root, configs.as_deref())
}

/// [`resolve_with_hosted`] for a caller that holds the hosted blobs and
/// should not copy them: it hands in each blob's `config_json`, in blob
/// order, and the rules are the same ones.
pub fn resolve_with_configs(
    root: &Path,
    configs: Option<&[Option<&str>]>,
) -> Result<Vec<Config>, String> {
    scan_services_with_hosted(root, configs).map(|scan| scan.services)
}

fn scan_services_with_hosted(
    root: &Path,
    configs: Option<&[Option<&str>]>,
) -> Result<ScanServices, String> {
    let declared = has_root_config(root)?;
    if !declared && let Some(services) = configs.and_then(|configs| hosted_services(root, configs))
    {
        tracing::debug!(
            "Services for {}: the hosted index's configs ({})",
            root.display(),
            services.len()
        );
        return Ok(ScanServices {
            services,
            declared,
            without_source: Vec::new(),
        });
    }
    let derived = resolve(root)?;
    tracing::debug!("Services for {}: {}", root.display(), derived.reason);
    Ok(ScanServices {
        services: derived.services,
        declared,
        without_source: derived.without_source,
    })
}

fn has_root_config(root: &Path) -> Result<bool, String> {
    // The same test `resolve` makes: a file in the tree always wins.
    let config_path = root.join("carrick.json");
    match std::fs::symlink_metadata(&config_path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Could not read {}: {e}", config_path.display())),
    }
}

fn hosted_services(root: &Path, configs: &[Option<&str>]) -> Option<Vec<Config>> {
    if configs.is_empty() {
        return None;
    }
    let mut services = configs
        .iter()
        .map(|config| serde_json::from_str::<Config>((*config)?).ok())
        .collect::<Option<Vec<_>>>()?;
    services.sort_by(|a, b| a.service_name.cmp(&b.service_name));
    validate(root, &services).ok()?;
    Config::resolve_nested(&mut services);
    Some(services)
}

/// Resolve once, including the validation CI applies before scanning any files.
pub fn resolve(root: &Path) -> Result<ServiceDerivation, String> {
    let config_path = root.join("carrick.json");
    // symlink_metadata also sees broken symlinks: an existing path is never
    // mistaken for permission to infer and replace an explicit declaration.
    match std::fs::symlink_metadata(&config_path) {
        Ok(_) => {
            let mut services = Config::load_services(vec![config_path.clone()]).map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidInput {
                    e.to_string()
                } else {
                    format!("Failed to parse {}: {e}", config_path.display())
                }
            })?;
            // A line in an untracked file points at nothing the repo states
            // (a generated config), so only a tracked `carrick.json` keeps its
            // lines. No git, or a git error, drops them too.
            let tracked = crate::git_state::tracked_paths(root, &["carrick.json"])
                .is_ok_and(|paths| paths.iter().any(|path| path == "carrick.json"));
            if !tracked {
                for service in services.iter_mut() {
                    for block in service.declared_operations.iter_mut() {
                        for operation in block.operations.iter_mut() {
                            operation.line = None;
                        }
                    }
                }
            }
            validate(root, &services)?;
            let members = member_facts(root, &services);
            return Ok(ServiceDerivation {
                reason: "carrick.json".into(),
                config: serde_json::Value::Null,
                members,
                services,
                warnings: Vec::new(),
                notes: Vec::new(),
                without_source: Vec::new(),
                skipped_patterns: Vec::new(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Could not read {}: {e}", config_path.display())),
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let sources = workspace_patterns(&root)?;
    let mut members: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let mut skipped_patterns: Vec<SkippedPattern> = Vec::new();
    let mut reasons = sources
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let mut glob_deno_members = false;
    if !sources.is_empty() {
        let globs = WorkspaceGlobs::compile(&sources)?;
        let mut seen = vec![false; globs.positive.len()];
        let walker = walkdir::WalkDir::new(&root)
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
                    // A checkout of its own inside the repository is no
                    // member of this workspace: a linked worktree holds a
                    // copy of every member (carrick#1902).
                    && crate::file_finder::git_boundary(entry).is_none()
            });
        let mut deno_configs: Vec<PathBuf> = Vec::new();
        for entry in walker {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().is_file()
                && (entry.file_name() == "deno.json" || entry.file_name() == "deno.jsonc")
            {
                deno_configs.push(entry.path().to_path_buf());
                continue;
            }
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
            for index in globs.matching(relative) {
                seen[index] = true;
                matched = true;
            }
            if matched && !globs.excludes(relative) {
                members.insert(directory.to_path_buf(), entry.path().to_path_buf());
            }
        }
        for manifest in deno_apps_under_globs(&root, &globs, &members, &deno_configs, &mut seen)? {
            let directory = manifest.parent().expect("manifest parent").to_path_buf();
            members.entry(directory).or_insert(manifest);
            glob_deno_members = true;
        }
        for ((raw, _), seen) in globs.positive.iter().zip(seen) {
            // A folder emptied or optional in a real monorepo: nothing to
            // propose for it, which is not a reason to refuse the rest.
            if !seen
                && !skipped_patterns
                    .iter()
                    .any(|skipped| skipped.pattern == *raw)
            {
                skipped_patterns.push(SkippedPattern {
                    pattern: raw.clone(),
                    reason: "matches no package".into(),
                });
            }
        }
    }
    let deno = deno_workspace_manifest_paths(&root).map_err(|e| e.to_string())?;
    let has_deno = !deno.is_empty() || glob_deno_members;
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
    let without_source = leave_out_members_without_source(&root, &mut members);
    if members.is_empty() {
        if !sources.is_empty() && sources.iter().any(|(_, patterns)| !patterns.is_empty()) {
            return Err("Workspace patterns select no services; declare the intended services in carrick.json.".into());
        }
        members.insert(root.clone(), root.join("package.json"));
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
        let name = facts
            .and_then(|facts| facts.package.name)
            .filter(|_| !relative.as_os_str().is_empty())
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
            // A lone root `jsconfig.json` is named too, so the derived entry
            // states the config explicitly; both sides then read every file
            // of the service under it (carrick#3497).
            crate::module_aliases::service_config(&root, relative, None)
                .or_else(|| {
                    let jsconfig = relative.join("jsconfig.json");
                    root.join(&jsconfig).is_file().then_some(jsconfig)
                })
                .map(|config| relative_to_directory(&config, relative))
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
    // Both of these are true of every proposal of this shape and neither is
    // something this run found, so they are notes rather than warnings: they
    // belong to the document the agent reads and to the quickstart, not to a
    // terminal line with a warning marker on it (carrick#1032).
    let warnings: Vec<String> = skipped_patterns
        .iter()
        .map(|skipped| {
            format!(
                "Workspace pattern '{}' {}, so Carrick skips it.",
                skipped.pattern, skipped.reason
            )
        })
        .collect();
    let mut notes = Vec::new();
    if services.len() > 1 {
        notes.push("Workspace packages are proposed as services. Review service boundaries and shared source includes in carrick.json.".into());
    }
    if has_deno {
        notes.push("Deno services use their existing manifests and require Deno on PATH for type resolution.".into());
    }
    notes.extend(
        without_source
            .iter()
            .map(|directory| format!("skipped_no_source: {directory}")),
    );
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
        without_source,
        skipped_patterns,
    })
}

/// The Deno apps a Node workspace glob holds without a `package.json`
/// (carrick#3969): a pnpm or npm glob names a folder (`apps/*` holds
/// `apps/edge`) whose app is a Deno project, configured by its own
/// `deno.json`/`deno.jsonc` at that folder or below it, with no root Deno
/// workspace listing it. The package manager never sees it, so the
/// `package.json` walk does not either.
///
/// The marker is the Deno config file alone, because it is what
/// [`crate::deno_support::service_manifest`] reads to scan the service as
/// Deno. Each config counts when it sits inside no `package.json` member,
/// some folder from it up to the root is held by a glob, it is under no test
/// folder, and no other such config sits above it: the nearest config is the
/// app, and one nested inside it is the app's own. A folder with no Deno
/// config stays out, as before.
fn deno_apps_under_globs(
    root: &Path,
    globs: &WorkspaceGlobs,
    members: &BTreeMap<PathBuf, PathBuf>,
    deno_configs: &[PathBuf],
    seen: &mut [bool],
) -> Result<Vec<PathBuf>, String> {
    let mut apps: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for config in deno_configs {
        let directory = config.parent().expect("config parent");
        if directory.join("package.json").is_file()
            || members.keys().any(|member| directory.starts_with(member))
            || crate::file_finder::is_under_test_dir(config, root)
        {
            continue;
        }
        let relative = directory.strip_prefix(root).map_err(|e| e.to_string())?;
        let held: Vec<usize> = relative
            .ancestors()
            .filter(|folder| !folder.as_os_str().is_empty() && !globs.excludes(folder))
            .flat_map(|folder| globs.matching(folder).collect::<Vec<_>>())
            .collect();
        if held.is_empty() {
            continue;
        }
        for index in held {
            seen[index] = true;
        }
        // `deno.json` and `deno.jsonc` in one folder: the one Deno reads.
        if let Some(manifest) = crate::deno_support::manifest_at(directory) {
            apps.insert(directory.to_path_buf(), manifest);
        }
    }
    let directories: Vec<PathBuf> = apps.keys().cloned().collect();
    Ok(apps
        .into_iter()
        .filter(|(directory, _)| {
            !directories
                .iter()
                .any(|other| other != directory && directory.starts_with(other))
        })
        .map(|(_, manifest)| manifest)
        .collect())
}

/// Take out of `members` every workspace member that holds no source file,
/// and return their repo-relative directories (carrick#2298).
///
/// A package manager's member is a directory with a manifest. A service is
/// source: a member that holds none (a configuration package, a package of
/// assets) has nothing a scan can index, and as a service it failed the scan
/// of every other member.
///
/// When no member holds source, none is taken out: nothing says which
/// directory was meant, and the scan reports the tree as it does a repository
/// with no source at all.
fn leave_out_members_without_source(
    root: &Path,
    members: &mut BTreeMap<PathBuf, PathBuf>,
) -> Vec<String> {
    let directories: BTreeSet<PathBuf> = members.keys().cloned().collect();
    let without: Vec<PathBuf> = directories
        .iter()
        .filter(|directory| !holds_source(directory, &directories))
        .cloned()
        .collect();
    if without.len() == directories.len() {
        return Vec::new();
    }
    without
        .into_iter()
        .map(|directory| {
            members.remove(&directory);
            let relative = directory.strip_prefix(root).unwrap_or(&directory);
            if relative.as_os_str().is_empty() {
                ".".to_string()
            } else {
                relative.to_string_lossy().replace('\\', "/")
            }
        })
        .collect()
}

/// Whether `directory` holds a file with one of the extensions a scan reads
/// ([`crate::file_finder::SOURCE_EXTENSIONS`], the list the npm manifest
/// publishes as `carrickInputs.source_extensions`), outside the other
/// members' own directories. Stops at the first one.
///
/// The extension alone, as the published rule is: whether the scan's walk
/// then reads the file (a test, a path git ignores, an `exclude`) is the
/// walk's question, and the scan answers it for each service nobody declared.
fn holds_source(directory: &Path, members: &BTreeSet<PathBuf>) -> bool {
    walkdir::WalkDir::new(directory)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || (entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name != ".git" && !MANIFEST_SKIP_DIRS.contains(&name))
                    && !members.contains(entry.path()))
        })
        .flatten()
        .any(|entry| {
            entry.file_type().is_file() && crate::file_finder::has_source_extension(entry.path())
        })
}

/// The manifest a service's own directory holds, npm's before Deno's.
fn member_manifest(root: &Path, service: &Config) -> Option<PathBuf> {
    let directory = root.join(service.directory.as_deref().unwrap_or("."));
    let package = directory.join("package.json");
    if package.is_file() {
        return Some(package);
    }
    crate::deno_support::manifest_at(&directory)
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

/// `config`, a repo-relative file, written as a path from `directory`, the
/// repo-relative service directory: `..` for each directory of `directory` the
/// file is not under, then the rest of the file's path.
fn relative_to_directory(config: &Path, directory: &Path) -> String {
    let shared = config
        .components()
        .zip(directory.components())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out = PathBuf::new();
    for _ in shared..directory.components().count() {
        out.push("..");
    }
    for part in config.components().skip(shared) {
        out.push(part);
    }
    out.to_string_lossy().replace('\\', "/")
}

/// Whether a path `carrick.json` names, relative to the repository root,
/// leaves the repository (carrick#2245): absolute, stepping up through `..`
/// (refused outright, as a workspace pattern is), or resolving through a
/// symlink to somewhere outside it. `root` is canonical. A path that does not
/// exist is judged by its text alone; the existence check reports it.
fn leaves_repository(root: &Path, path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        || crate::file_finder::resolves_outside(root, &root.join(path))
}

/// Every path a service declares must name something inside the repository,
/// because the scan reads what it names and carries it into the index
/// (carrick#2245).
fn validate(root: &Path, services: &[Config]) -> Result<(), String> {
    // Canonical, so a symlink is judged by where it lands and a root reached
    // through a symlink of its own (a macOS temp dir) still contains itself.
    let canonical = root
        .canonicalize()
        .map_err(|e| format!("Could not read {}: {e}", root.display()))?;
    for service in services {
        let label = service
            .service_name
            .as_deref()
            .or(service.directory.as_deref())
            .unwrap_or("<unnamed>");
        if let Some(directory) = &service.directory
            && leaves_repository(&canonical, directory)
        {
            return Err(format!(
                "Service '{label}' in {} declares directory '{directory}', which is outside the repository '{}'. Name a directory inside the repository, relative to carrick.json.",
                root.join("carrick.json").display(),
                root.display()
            ));
        }
        if let Some(include) = service
            .include
            .iter()
            .find(|include| leaves_repository(&canonical, include))
        {
            return Err(format!(
                "Service '{label}' declares include path '{include}', which is outside the repository '{}'. Name a path inside the repository, relative to carrick.json.",
                root.display()
            ));
        }
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
            // Relative to the service directory, so `../tsconfig.base.json`
            // is an ordinary shared config: judged by where it lands.
            if !path
                .canonicalize()
                .is_ok_and(|resolved| resolved.starts_with(&canonical))
            {
                return Err(format!(
                    "Service '{label}' declares tsconfig '{tsconfig}', which is outside the repository '{}'. Name a tsconfig inside the repository, relative to the service directory.",
                    root.display()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::CloudRepoData;
    use serde_json::json;

    /// An npm workspace of two members, `packages/a` and `packages/b`, whose
    /// manifests derive the split `@scope/a` and `@scope/b`.
    fn workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        for name in ["a", "b"] {
            let dir = tmp.path().join("packages").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("package.json"),
                format!(r#"{{"name":"@scope/{name}","version":"1.0.0"}}"#),
            )
            .unwrap();
        }
        tmp
    }

    /// A hosted blob carrying the config its service was indexed with.
    fn blob(config: Option<serde_json::Value>) -> CloudRepoData {
        serde_json::from_value(json!({
            "repo_name":"repo", "endpoints":[],"calls":[],"mounts":[],"apps":{},
            "imported_handlers":[],"function_definitions":{},
            "last_updated":"2026-10-10T10:00:00Z","commit_hash":"abc",
            "file_results":{},
            "config_json": config.map(|c| c.to_string()),
        }))
        .unwrap()
    }

    /// Served out of order, so the sort is what puts `a` first.
    fn hosted() -> Vec<CloudRepoData> {
        vec![
            blob(Some(
                json!({"serviceName":"b","directory":"packages/b","internalEnvVars":["API_URL"]}),
            )),
            blob(Some(json!({
                "serviceName":"a","directory":"packages/a",
                "internalEnvVars":["API_URL"],"exclude":["generated/**"]
            }))),
        ]
    }

    fn names(services: &[Config]) -> Vec<&str> {
        services
            .iter()
            .map(|s| s.service_name.as_deref().unwrap_or_default())
            .collect()
    }

    #[test]
    fn a_no_model_re_read_takes_the_hosted_configs() {
        let tmp = workspace();
        let services = resolve_gated(tmp.path(), true, || Some(hosted()))
            .unwrap()
            .services;
        assert_eq!(names(&services), ["a", "b"]);
        assert!(services[0].internal_env_vars.contains("API_URL"));
        assert!(services[1].internal_env_vars.contains("API_URL"));
        assert_eq!(services[0].exclude, ["generated/**"]);
    }

    #[test]
    fn a_root_carrick_json_wins_over_the_hosted_configs() {
        let tmp = workspace();
        std::fs::write(
            tmp.path().join("carrick.json"),
            r#"{"services":[{"name":"declared","directory":"packages/a"}]}"#,
        )
        .unwrap();
        let services = resolve_gated(tmp.path(), true, || Some(hosted()))
            .unwrap()
            .services;
        assert_eq!(names(&services), ["declared"]);
        let services = resolve_with_hosted(tmp.path(), Some(&hosted())).unwrap();
        assert_eq!(names(&services), ["declared"]);
    }

    #[test]
    fn a_scan_that_asks_the_model_derives_from_the_tree() {
        let tmp = workspace();
        let services = resolve_gated(tmp.path(), false, || Some(hosted()))
            .unwrap()
            .services;
        assert_eq!(names(&services), ["@scope/a", "@scope/b"]);
    }

    #[test]
    fn one_blob_without_a_config_falls_back_to_the_derived_split() {
        let tmp = workspace();
        let mut blobs = hosted();
        blobs[1].config_json = None;
        let services = resolve_gated(tmp.path(), true, || Some(blobs))
            .unwrap()
            .services;
        assert_eq!(names(&services), ["@scope/a", "@scope/b"]);
        assert!(services.iter().all(|s| s.internal_env_vars.is_empty()));
    }

    #[test]
    fn no_hosted_blobs_or_a_directory_this_tree_lacks_falls_back() {
        let tmp = workspace();
        let derived = ["@scope/a", "@scope/b"];
        let services = resolve_gated(tmp.path(), true, || None).unwrap().services;
        assert_eq!(names(&services), derived);
        let services = resolve_gated(tmp.path(), true, || Some(Vec::new()))
            .unwrap()
            .services;
        assert_eq!(names(&services), derived);
        let mut blobs = hosted();
        blobs.push(blob(Some(
            json!({"serviceName":"c","directory":"packages/c"}),
        )));
        let services = resolve_gated(tmp.path(), true, || Some(blobs))
            .unwrap()
            .services;
        assert_eq!(names(&services), derived);
    }

    #[test]
    fn hosted_services_nested_inside_another_are_resolved() {
        let tmp = workspace();
        let blobs = vec![
            blob(Some(json!({"serviceName":"root"}))),
            blob(Some(json!({"serviceName":"a","directory":"packages/a"}))),
        ];
        let services = resolve_gated(tmp.path(), true, || Some(blobs))
            .unwrap()
            .services;
        assert_eq!(names(&services), ["a", "root"]);
        assert_eq!(services[1].nested_directories, ["packages/a"]);
    }

    fn git_in(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn operation_lines(derivation: &ServiceDerivation) -> Vec<Option<u32>> {
        derivation
            .services
            .iter()
            .flat_map(|service| &service.declared_operations)
            .flat_map(|block| &block.operations)
            .map(|operation| operation.line)
            .collect()
    }

    /// carrick-cloud#3422 S2: only a tracked `carrick.json` keeps the lines of
    /// its declared operations; an untracked (generated) one points at nothing
    /// the repo states.
    #[test]
    fn declared_operation_lines_need_a_tracked_carrick_json() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git_in(repo, &["init", "-q"]);
        std::fs::write(
            repo.join("carrick.json"),
            r#"{
  "services": [{ "name": "rpc", "directory": "." }],
  "operations": [
    {
      "service": "rpc",
      "route": "POST /rpc",
      "dispatch": { "location": "body", "field": "kind" },
      "operations": [{ "value": "a" }]
    }
  ]
}
"#,
        )
        .unwrap();

        let untracked = resolve(repo).unwrap();
        assert_eq!(operation_lines(&untracked), vec![None]);

        git_in(repo, &["add", "carrick.json"]);
        let tracked = resolve(repo).unwrap();
        assert_eq!(operation_lines(&tracked), vec![Some(8)]);
    }
}
