//! Asking the library store what a service's packages are (carrick#1664,
//! build ticket 10 of carrick#1616).
//!
//! The store answers, per imported registry package and installed version,
//! what each export is and how it is used ([`crate::library_claims`]). The
//! request and the answer are pinned on carrick#1564 (comment 5937606126,
//! section 2) and amendment 1 (comment 5938965179):
//!
//! - **What is asked.** The registry packages the service makes library calls
//!   through, each at the version its install holds, with the specifiers it
//!   imports it by, and the runtime modules it imports (`node:events`), at
//!   the version of the runtime's types package (A1). In-repo packages are
//!   carrick#1666's (amendment 4), and are never sent here.
//! - **Privacy (A2).** A package is sent only when its install recorded it as
//!   coming from the public npm registry ([`from_public_registry`]): the store
//!   asks npm about every name it receives, so a private name would reach the
//!   public registry. Anything this cannot prove is not sent.
//! - **The answer** is read one element at a time ([`parse_answer`]) and can
//!   never fail a scan. An entry the store has not finished is `pending`. One
//!   whose build is queued, in flight or starting ([`WAITED_REASONS`]) is
//!   asked again every [`POLL_INTERVAL`] for up to [`WAIT_CAP`] from the first
//!   ask ([`ask_until_settled`], carrick#2496); any other pending package is
//!   named at the end and asked again next scan.
//! - **Transport (A5)** is the agent service's one path
//!   ([`crate::agent_service::AgentService::post_to_lambda`]): the scan slot,
//!   the scanner version, the pacer. The route's 2xx body is the answer
//!   itself, not the envelope
//!   ([`crate::agent_service::SuccessBody::of`]; ruled on carrick#1664,
//!   2026-10-02). Any failure, a
//!   `403 scan_not_started` or a throttled `429` included, means no claims
//!   this scan, and the scan carries on.
//!
//! The rules are in `docs/reference/client-semantics.md`, "Message roles".

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tracing::{debug, warn};

use crate::agent_service::AgentCallError;
use crate::library_claims::{CallClaim, ExportClaims, Side, SideRule};
use crate::services::type_sidecar::{LibraryClaim, LibraryRole};

/// The roles this scanner reads rows for: the store spends nothing on others.
pub const ROLES: [&str; 3] = ["broker", "socket", "in_process_bus"];

/// Every network role the store answers (carrick-cloud#2377), asked when
/// site questions are on ([`crate::site_questions::enabled`], carrick#2436):
/// discovery and the site labels read all of them. The message roles come
/// first, in [`ROLES`]' order. Library rows still read the message roles
/// only ([`crate::library_claims::read`]), so a claim of another role feeds
/// discovery and the site rows and states no library row.
pub const NETWORK_ROLES: [&str; 6] = [
    "broker",
    "socket",
    "in_process_bus",
    "http_client",
    "graphql_client",
    "server_framework",
];

/// The roles a scan asks the store for: every network role with site
/// questions on, the message roles alone with them off.
pub fn roles_for(site_questions: bool) -> &'static [&'static str] {
    if site_questions {
        &NETWORK_ROLES
    } else {
        &ROLES
    }
}

/// The store's route.
pub const ROUTE: &str = "/library-claims";

/// The offline answer's key: `<CARRICK_MOCK_FIXTURE_DIR>/library-claims/default.json`.
pub const MOCK_SEED: &str = "default";

/// How long one ask may take before the scan goes on without it.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long between two asks for the packages the store is still building.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long, from the first ask, the scan waits for those packages.
pub const WAIT_CAP: Duration = Duration::from_secs(600);

/// The store's pending reasons (carrick-cloud `lambdas/library-claims/http.ts`
/// `missing`) that a wait can end: a build is running (`in_flight`) or is
/// about to be started by this very ask (`building`). Every other pending
/// reason is a state asking again does not change within a scan: a stage the
/// store has no pin for (`classifier_unavailable`, `picker_unavailable`,
/// `lister_unavailable`), a store it could not read (`store_unavailable`,
/// `store_row_unreadable`) or a build it could not dispatch
/// (`dispatch_failed`). A failed build is not pending at all: the store
/// answers it `skipped` (`build_failed`, due a retry later).
pub const WAITED_REASONS: [&str; 2] = ["in_flight", "building"];

/// The most registry packages one request carries (the store's cap).
const MAX_PACKAGES: usize = 200;

/// The most specifiers one package carries (the store trims past it).
const MAX_SPECIFIERS: usize = 32;

/// The registries a package may come from to be sent (A2).
const PUBLIC_REGISTRIES: [&str; 2] = ["registry.npmjs.org", "registry.yarnpkg.com"];

/// The runtime's types package: a runtime module's version is its version.
pub(crate) const RUNTIME_TYPES: &str = "@types/node";

/// The lockfiles an install writes.
const LOCKFILES: [&str; 5] = [
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "deno.lock",
];

/// A request to the store (contract section 2). `workspace_packages` is
/// carrick#1666's and is never part of it here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryClaimsRequest {
    pub roles: Vec<&'static str>,
    pub packages: Vec<RegistryPackage>,
}

/// One package the store is asked about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryPackage {
    /// The registry package, or `node:<module>` for a runtime module.
    pub package: String,
    /// The installed version (for a runtime module, the runtime's types
    /// package's).
    pub version: String,
    /// The specifiers the service imports it by. A runtime module's is its
    /// `node:` form, the only one the store takes.
    pub specifiers: Vec<String>,
    /// For a `lib:<file>` entry, the globals the code uses from that file:
    /// the whole file lists more than the store's cap holds (carrick#2504).
    /// Omitted when empty, so a request without one is byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<String>,
}

/// Where a service's install lives: what the A2 rule reads.
#[derive(Debug, Clone, Copy)]
pub struct Install<'a> {
    /// The service's root: its `node_modules` is searched first.
    pub service_root: &'a Path,
    /// The repo's root: the highest `node_modules` searched.
    pub repo_root: &'a Path,
    /// The user's home directory, whose `.npmrc` and `.yarnrc.yml` an
    /// install also reads.
    pub home: Option<&'a Path>,
    /// `YARN_NPM_REGISTRY_SERVER`, which outranks every `.yarnrc.yml`.
    pub yarn_registry_env: Option<&'a str>,
    /// `NPM_CONFIG_REGISTRY`, the default registry Deno installs from when
    /// it is set.
    pub npm_registry_env: Option<&'a str>,
}

/// What a service asks, and how to read the answer back onto its imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub request: LibraryClaimsRequest,
    /// Every runtime module asked about (`node:events`), with each spelling
    /// the service imports it by (`events`, `node:events`): the store answers
    /// its exports under the `node:` form only.
    spellings: BTreeMap<String, BTreeSet<String>>,
}

impl Asked {
    /// A request naming `packages` at 1.0.0, for tests that drive the wait
    /// without an install.
    #[cfg(test)]
    pub(crate) fn naming(packages: &[&str]) -> Self {
        Self::naming_for(packages, &ROLES)
    }

    /// [`Self::naming`], asking `roles`.
    #[cfg(test)]
    pub(crate) fn naming_for(packages: &[&str], roles: &[&'static str]) -> Self {
        Self {
            request: LibraryClaimsRequest {
                roles: roles.to_vec(),
                packages: packages
                    .iter()
                    .map(|name| RegistryPackage {
                        package: name.to_string(),
                        version: "1.0.0".to_string(),
                        specifiers: vec![name.to_string()],
                        exports: Vec::new(),
                    })
                    .collect(),
            },
            spellings: BTreeMap::new(),
        }
    }

    /// This request plus `library` (the language's own library, from
    /// [`default_library_packages`]). A service that asks about no package
    /// still asks about the library, so `asked` may be `None`.
    pub fn with_default_library(
        asked: Option<Self>,
        library: Vec<RegistryPackage>,
    ) -> Option<Self> {
        if library.is_empty() {
            return asked;
        }
        let mut asked = asked.unwrap_or_else(|| Self {
            request: LibraryClaimsRequest {
                roles: ROLES.to_vec(),
                packages: Vec::new(),
            },
            spellings: BTreeMap::new(),
        });
        let room = MAX_PACKAGES.saturating_sub(asked.request.packages.len());
        asked
            .request
            .packages
            .extend(library.into_iter().take(room));
        Some(asked)
    }

    /// `own` plus what each of `siblings` asks that it does not
    /// (carrick#2618): a walk that enters a sibling service's code reads its
    /// calls through the sibling's packages, which only an answer for them
    /// labels. The service's own entries come first, so the cap drops a
    /// sibling's, and the count of those dropped comes back. A package at the
    /// same version is asked once, with every specifier either imports it by.
    pub fn merged(
        own: Option<Self>,
        siblings: impl IntoIterator<Item = Self>,
    ) -> (Option<Self>, usize) {
        let mut dropped = 0;
        let mut merged = own;
        for other in siblings {
            let Some(asked) = merged.as_mut() else {
                merged = Some(other);
                continue;
            };
            for package in other.request.packages {
                let known = asked.request.packages.iter().position(|known| {
                    known.package == package.package && known.version == package.version
                });
                match known {
                    Some(at) => {
                        let known = &mut asked.request.packages[at];
                        for specifier in package.specifiers {
                            if known.specifiers.len() < MAX_SPECIFIERS
                                && !known.specifiers.contains(&specifier)
                            {
                                known.specifiers.push(specifier);
                            }
                        }
                        for export in package.exports {
                            if !known.exports.contains(&export) {
                                known.exports.push(export);
                            }
                        }
                    }
                    None if asked.request.packages.len() < MAX_PACKAGES => {
                        asked.request.packages.push(package);
                    }
                    None => dropped += 1,
                }
            }
            for (module, spellings) in other.spellings {
                asked.spellings.entry(module).or_default().extend(spellings);
            }
        }
        (merged, dropped)
    }

    /// The same packages, asking `roles` instead.
    pub fn with_roles(mut self, roles: &[&'static str]) -> Self {
        self.request.roles = roles.to_vec();
        self
    }

    /// The request for only `packages`, or `None` when it names none of
    /// them.
    fn only(&self, packages: &BTreeSet<String>) -> Option<LibraryClaimsRequest> {
        let kept: Vec<RegistryPackage> = self
            .request
            .packages
            .iter()
            .filter(|package| packages.contains(&package.package))
            .cloned()
            .collect();
        (!kept.is_empty()).then(|| LibraryClaimsRequest {
            roles: self.request.roles.clone(),
            packages: kept,
        })
    }

    /// The exports of `answered` the service can use: those of a package at
    /// the version it asked about, a runtime module's stated once per
    /// spelling the service imports it by. Anything else names something
    /// this service did not ask about, and is dropped.
    fn claims(&self, answered: Vec<ExportClaims>) -> Vec<ExportClaims> {
        let mut claims = Vec::new();
        for export in answered {
            let asked = self.request.packages.iter().any(|package| {
                package.package == export.package && package.version == export.version
            });
            if !asked {
                continue;
            }
            match self.spellings.get(&export.package) {
                Some(spellings) if export.specifier == export.package => {
                    for spelling in spellings {
                        claims.push(ExportClaims {
                            specifier: spelling.clone(),
                            ..export.clone()
                        });
                    }
                }
                Some(_) => {}
                None => claims.push(export),
            }
        }
        claims
    }

    /// The packages of `answer` the store answered for this service: those
    /// answered at the version it asked about, as [`Self::claims`] keeps
    /// their exports. A package answered for nothing this service asked is
    /// no package it can tell answered (carrick#2725): with its exports
    /// dropped, a call into it would read as answered with nothing to label.
    fn answered(&self, answer: &LibraryAnswer) -> BTreeSet<String> {
        self.request
            .packages
            .iter()
            .filter(|package| {
                answer
                    .answered_versions
                    .contains(&(package.package.clone(), package.version.clone()))
            })
            .map(|package| package.package.clone())
            .collect()
    }
}

/// What a service asks the store, given every specifier it makes a library
/// call through ([`crate::request_summary::LibrarySites::packages`]). `None`
/// when nothing qualifies. A specifier no installed package answers (a path
/// alias, a package not installed), a package its install did not take from
/// the public registry, a runtime module's subpath (`fs/promises`), and a
/// runtime module with no runtime types package installed are all left out.
pub fn request(specifiers: &BTreeSet<String>, install: &Install<'_>) -> Option<Asked> {
    let declared = declared_names(install.service_root, install.repo_root);
    let runtime = installed(RUNTIME_TYPES, install);
    let mut by_package: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    let mut spellings: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for specifier in specifiers {
        let Some(asked) = asked_as(specifier, &declared, runtime.as_ref(), install) else {
            continue;
        };
        let wire = if asked.runtime {
            spellings
                .entry(asked.package.clone())
                .or_default()
                .insert(specifier.clone());
            asked.package.clone()
        } else {
            specifier.clone()
        };
        by_package
            .entry(asked.package)
            .or_insert_with(|| (asked.version, BTreeSet::new()))
            .1
            .insert(wire);
    }
    let packages: Vec<RegistryPackage> = by_package
        .into_iter()
        .take(MAX_PACKAGES)
        .map(|(package, (version, specifiers))| RegistryPackage {
            package,
            version,
            specifiers: specifiers.into_iter().take(MAX_SPECIFIERS).collect(),
            exports: Vec::new(),
        })
        .collect();
    (!packages.is_empty()).then(|| Asked {
        request: LibraryClaimsRequest {
            roles: ROLES.to_vec(),
            packages,
        },
        spellings,
    })
}

/// The language's own library as the store is asked about it (carrick#2504):
/// one `lib:<file>` package per default-library file the program loads, at
/// the compiler's version, narrowed to the globals the code uses from it.
/// `globals` is by library file (`lib.dom.d.ts`).
pub fn default_library_packages(
    typescript: &str,
    globals: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<RegistryPackage> {
    globals
        .iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(file, names)| {
            let key = format!("lib:{file}");
            RegistryPackage {
                package: key.clone(),
                version: typescript.to_string(),
                specifiers: vec![key],
                exports: names.iter().cloned().collect(),
            }
        })
        .collect()
}

/// The globals the runtime's own types declare, as the store is asked about
/// them (carrick#2445): one package per declaring file, keyed as a runtime
/// module is (`node:web-globals/fetch`), at the installed runtime types'
/// version, narrowed to the globals the code uses from it. `globals` is by
/// that key ([`crate::request_summary::discovery::runtime_globals`]). None
/// when no runtime types are installed.
pub fn runtime_global_packages(
    globals: &BTreeMap<String, BTreeSet<String>>,
    install: &Install<'_>,
) -> Vec<RegistryPackage> {
    let Some(runtime) = installed(RUNTIME_TYPES, install) else {
        return Vec::new();
    };
    globals
        .iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(key, names)| RegistryPackage {
            package: key.clone(),
            version: runtime.version.clone(),
            specifiers: vec![key.clone()],
            exports: names.iter().cloned().collect(),
        })
        .collect()
}

/// The entry one specifier is asked under.
struct AskedAs {
    package: String,
    version: String,
    runtime: bool,
}

/// The package and version `specifier` is asked about, if it is asked about
/// at all (A1, A2).
fn asked_as(
    specifier: &str,
    declared: &BTreeSet<String>,
    runtime: Option<&Installed>,
    install: &Install<'_>,
) -> Option<AskedAs> {
    let runtime_module = |module: &str| {
        let runtime = runtime?;
        (!module.is_empty()
            && !module.contains('/')
            && runtime.dir.join(format!("{module}.d.ts")).is_file())
        .then(|| AskedAs {
            package: format!("node:{module}"),
            version: runtime.version.clone(),
            runtime: true,
        })
    };
    // `node:events`: the runtime's module, at its types package's version.
    if let Some(module) = specifier.strip_prefix("node:") {
        return runtime_module(module);
    }
    // `events` written bare is the runtime's module only when the service
    // declares no package of that name (A1).
    if !declared.contains(specifier)
        && let Some(asked) = runtime_module(specifier)
    {
        return Some(asked);
    }
    let name = crate::request_summary::package_name(specifier);
    let package = installed(name, install)?;
    from_public_registry(name, &package, install).then(|| AskedAs {
        package: name.to_string(),
        version: package.version,
        runtime: false,
    })
}

/// A package's installed copy: its directory and the version its manifest
/// states.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Installed {
    dir: PathBuf,
    version: String,
    /// The directory whose `node_modules` holds it: where its install ran.
    install_root: PathBuf,
}

/// The installed copy of `name` the service resolves: the first
/// `node_modules/<name>/package.json` stating a version, from the service
/// root up to the repo root.
fn installed(name: &str, install: &Install<'_>) -> Option<Installed> {
    roots(install.service_root, install.repo_root)
        .into_iter()
        .find_map(|root| {
            let dir = root.join("node_modules").join(name);
            let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
            let manifest: Value = serde_json::from_str(&text).ok()?;
            let version = manifest.get("version")?.as_str()?.to_string();
            Some(Installed {
                dir,
                version,
                install_root: root,
            })
        })
}

/// The directories from `from` up to `repo_root`, both included; only `from`
/// when it is not inside the repo root.
fn roots(from: &Path, repo_root: &Path) -> Vec<PathBuf> {
    if !from.starts_with(repo_root) {
        return vec![from.to_path_buf()];
    }
    from.ancestors()
        .take_while(|dir| dir.starts_with(repo_root))
        .map(Path::to_path_buf)
        .collect()
}

/// Every package name the service's manifest and the repo root's declare,
/// in any dependency list.
fn declared_names(service_root: &Path, repo_root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for root in [service_root, repo_root] {
        let Some(manifest) = std::fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        for list in [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ] {
            if let Some(Value::Object(deps)) = manifest.get(list) {
                names.extend(deps.keys().cloned());
            }
        }
    }
    names
}

/// Whether the install recorded `name` as taken from the public npm registry
/// (amendment 1, A2), read from what the install wrote: the lockfiles in the
/// nearest directory holding one, from where the package is installed up to
/// the repo root, and every `.npmrc` the install could have read. Fails
/// closed: no lockfile, an unreadable one, a package it does not list, a
/// scope or default registry mapped elsewhere, and a git, file, link,
/// workspace or tarball-URL dependency are all not public. Every lockfile in
/// that directory must agree.
fn from_public_registry(name: &str, package: &Installed, install: &Install<'_>) -> bool {
    let mut npmrcs: Vec<PathBuf> = roots(&package.install_root, install.repo_root);
    npmrcs.extend(roots(install.service_root, install.repo_root));
    npmrcs.extend(install.home.map(Path::to_path_buf));
    if npmrcs
        .iter()
        .any(|dir| npmrc_maps_elsewhere(&dir.join(".npmrc"), name))
    {
        return false;
    }
    let Some(lock_dir) = roots(&package.install_root, install.repo_root)
        .into_iter()
        .find(|dir| LOCKFILES.iter().any(|lock| dir.join(lock).is_file()))
    else {
        return false;
    };
    let answers: Vec<bool> = [
        npm_lock(name, package, &lock_dir, NpmLock::Hidden),
        npm_lock(name, package, &lock_dir, NpmLock::Committed),
        pnpm_lock(name, package, &lock_dir),
        yarn_lock(name, package, &lock_dir, install),
        deno_lock(name, package, &lock_dir, install),
    ]
    .into_iter()
    .flatten()
    .collect();
    !answers.is_empty() && answers.into_iter().all(|public| public)
}

/// Whether a URL is the public registry's: `https://`, on one of
/// [`PUBLIC_REGISTRIES`] exactly.
fn is_public_url(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix("https://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    PUBLIC_REGISTRIES.contains(&host.as_str())
}

/// `@scope` of a scoped package name.
fn scope_of(name: &str) -> Option<&str> {
    name.starts_with('@')
        .then(|| name.split('/').next())
        .flatten()
}

/// Whether an `.npmrc` maps the registry `name` installs from (its scope's,
/// or the default) anywhere but the public registry. A file that is absent
/// maps nothing.
fn npmrc_maps_elsewhere(path: &Path, name: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let scoped_key = scope_of(name).map(|scope| format!("{scope}:registry"));
    let mut scoped: Option<String> = None;
    let mut default: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim().trim_matches('"').to_string());
        if key == "registry" {
            default = Some(value);
        } else if scoped_key.as_deref() == Some(key) {
            scoped = Some(value);
        }
    }
    scoped
        .or(default)
        .is_some_and(|registry| !is_public_url(&registry))
}

/// Which npm lockfile is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NpmLock {
    /// `node_modules/.package-lock.json`, which the install itself writes.
    Hidden,
    /// `package-lock.json` or `npm-shrinkwrap.json`.
    Committed,
}

/// npm's answer from one of its lockfiles in `lock_dir`, or `None` when that
/// file is absent. The entry is the one at the installed copy's path from
/// the lockfile's directory; a v1 lockfile is read by name.
fn npm_lock(name: &str, package: &Installed, lock_dir: &Path, which: NpmLock) -> Option<bool> {
    let candidates = match which {
        NpmLock::Hidden => vec![lock_dir.join("node_modules").join(".package-lock.json")],
        NpmLock::Committed => vec![
            lock_dir.join("package-lock.json"),
            lock_dir.join("npm-shrinkwrap.json"),
        ],
    };
    let path = candidates.into_iter().find(|path| path.is_file())?;
    let Some(lock) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return Some(false);
    };
    let entry = match lock.get("packages") {
        Some(packages) => package
            .dir
            .strip_prefix(lock_dir)
            .ok()
            .and_then(|key| packages.get(key.to_string_lossy().replace('\\', "/"))),
        None => lock.get("dependencies").and_then(|deps| deps.get(name)),
    };
    // A linked workspace package's `resolved` is its path, which is no
    // public URL.
    let Some(entry) = entry else {
        return Some(false);
    };
    Some(
        entry
            .get("resolved")
            .and_then(Value::as_str)
            .is_some_and(is_public_url),
    )
}

/// One YAML file, parsed, or `None` when it is absent or unreadable.
fn read_yaml(path: &Path) -> Option<serde_yaml_ng::Value> {
    serde_yaml_ng::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// pnpm's answer from `pnpm-lock.yaml` and the install's
/// `node_modules/.modules.yaml`, or `None` when the lockfile is absent. The
/// registry pnpm recorded for the package's scope (or its default) must be
/// public, and the lockfile's entry for the installed version must resolve
/// by integrity, with any `tarball` on the public registry: a lockfile
/// written with `lockfile-include-tarball-url=true` records one for every
/// package (carrick#1721). A `tarball` elsewhere, a `type` (git, directory),
/// a `directory` or a `repo` names another source.
fn pnpm_lock(name: &str, package: &Installed, lock_dir: &Path) -> Option<bool> {
    let lock_path = lock_dir.join("pnpm-lock.yaml");
    if !lock_path.is_file() {
        return None;
    }
    let Some(modules) = read_yaml(&lock_dir.join("node_modules").join(".modules.yaml")) else {
        return Some(false);
    };
    let registries = modules.get("registries");
    let registry = scope_of(name)
        .and_then(|scope| registries?.get(scope))
        .or_else(|| registries?.get("default"))
        .and_then(serde_yaml_ng::Value::as_str);
    if !registry.is_some_and(is_public_url) {
        return Some(false);
    }
    let Some(serde_yaml_ng::Value::Mapping(packages)) =
        read_yaml(&lock_path).and_then(|lock| lock.get("packages").cloned())
    else {
        return Some(false);
    };
    // `name@1.2.3` and `name@1.2.3(peer@2.0.0)` (v9), `/name@1.2.3` (v6),
    // `/name/1.2.3` (v5).
    let wanted = [
        format!("{name}@{}", package.version),
        format!("{name}/{}", package.version),
    ];
    let entry = packages.iter().find_map(|(key, entry)| {
        let key = key.as_str()?.trim_start_matches('/');
        let bare = key.split('(').next().unwrap_or_default();
        wanted.iter().any(|wanted| wanted == bare).then_some(entry)
    });
    let Some(resolution) = entry.and_then(|entry| entry.get("resolution")) else {
        return Some(false);
    };
    Some(
        resolution.get("integrity").is_some()
            && resolution
                .get("tarball")
                .is_none_or(|tarball| tarball.as_str().is_some_and(is_public_url))
            && ["type", "directory", "repo"]
                .iter()
                .all(|field| resolution.get(*field).is_none()),
    )
}

/// Yarn's answer from `yarn.lock`, or `None` when it is absent. A classic
/// lockfile records where each package came from; a Berry one does not, so
/// its registry is read from configuration.
fn yarn_lock(
    name: &str,
    package: &Installed,
    lock_dir: &Path,
    install: &Install<'_>,
) -> Option<bool> {
    let path = lock_dir.join("yarn.lock");
    if !path.is_file() {
        return None;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Some(false);
    };
    if text.contains("__metadata:") {
        return Some(yarn_berry(name, package, lock_dir, &text, install));
    }
    Some(yarn_classic(name, &package.version, &text))
}

/// A classic `yarn.lock`: blocks of `"name@range", name@range:` headers,
/// each followed by indented `version "x"` and `resolved "url"` lines. The
/// block naming `name` at the installed version must resolve publicly.
fn yarn_classic(name: &str, version: &str, text: &str) -> bool {
    let mut names_it = false;
    let mut found_version: Option<String> = None;
    let mut resolved: Option<String> = None;
    let public = |names_it: bool, found: &Option<String>, resolved: &Option<String>| {
        names_it
            && found.as_deref() == Some(version)
            && resolved.as_deref().is_some_and(is_public_url)
    };
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') {
            if public(names_it, &found_version, &resolved) {
                return true;
            }
            names_it = line.trim_end_matches(':').split(',').any(|spec| {
                spec.trim()
                    .trim_matches('"')
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('@'))
            });
            found_version = None;
            resolved = None;
            continue;
        }
        let field = line.trim();
        if let Some(value) = field.strip_prefix("version ") {
            found_version = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = field.strip_prefix("resolved ") {
            resolved = Some(value.trim_matches('"').to_string());
        }
    }
    public(names_it, &found_version, &resolved)
}

/// A Berry `yarn.lock` (YAML). The installed version must resolve by the
/// `npm:` protocol, and the registry it is fetched from must be public in
/// every place Yarn reads one: each `.yarnrc.yml` from the lockfile's
/// directory up, the home one, and `YARN_NPM_REGISTRY_SERVER`. A scope
/// mapped in any of them is held to that mapping; otherwise every default
/// registry named must be public, and none named is Yarn's own default.
fn yarn_berry(
    name: &str,
    package: &Installed,
    lock_dir: &Path,
    text: &str,
    install: &Install<'_>,
) -> bool {
    let Ok(serde_yaml_ng::Value::Mapping(entries)) = serde_yaml_ng::from_str(text) else {
        return false;
    };
    let wanted = format!("{name}@npm:{}", package.version);
    let resolved_by_npm = entries.iter().any(|(_, entry)| {
        entry
            .get("resolution")
            .and_then(serde_yaml_ng::Value::as_str)
            == Some(wanted.as_str())
    });
    if !resolved_by_npm {
        return false;
    }
    let rcs: Vec<serde_yaml_ng::Value> = lock_dir
        .ancestors()
        .map(Path::to_path_buf)
        .chain(install.home.map(Path::to_path_buf))
        .filter_map(|dir| read_yaml(&dir.join(".yarnrc.yml")))
        .collect();
    let server = |value: Option<&serde_yaml_ng::Value>| {
        value
            .and_then(|value| value.get("npmRegistryServer"))
            .and_then(serde_yaml_ng::Value::as_str)
            .map(str::to_string)
    };
    let scoped: Vec<String> = scope_of(name)
        .map(|scope| scope.trim_start_matches('@'))
        .map(|scope| {
            rcs.iter()
                .filter_map(|rc| server(rc.get("npmScopes").and_then(|scopes| scopes.get(scope))))
                .collect()
        })
        .unwrap_or_default();
    if !scoped.is_empty() {
        return scoped.iter().all(|registry| is_public_url(registry));
    }
    rcs.iter()
        .filter_map(|rc| server(Some(rc)))
        .chain(install.yarn_registry_env.map(str::to_string))
        .all(|registry| is_public_url(&registry))
}

/// Deno's answer from `deno.lock`, or `None` when it is absent
/// (carrick#1720). The lockfile's npm section must list the installed
/// version with its integrity, and the lockfile must take the name from
/// nowhere else: not from JSR, where a bare import of it may resolve
/// instead, and not from a local folder the workspace links in its place.
///
/// A Deno lockfile records no host for a package fetched from the default
/// registry. Format 5 records a `tarball` for any other host, which must
/// then be public; formats 2 to 4 record none, so the registry is read where
/// Deno reads it: every `.npmrc` ([`from_public_registry`]) and
/// `NPM_CONFIG_REGISTRY`, which must be public when set. The formats are
/// read as Deno's own `deno_lockfile` crate (0.61.0) states them: format 1
/// holds no npm package, and a format this scanner does not know sends
/// nothing.
fn deno_lock(
    name: &str,
    package: &Installed,
    lock_dir: &Path,
    install: &Install<'_>,
) -> Option<bool> {
    let path = lock_dir.join("deno.lock");
    if !path.is_file() {
        return None;
    }
    if !install.npm_registry_env.is_none_or(is_public_url) {
        return Some(false);
    }
    let Some(lock) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return Some(false);
    };
    let (npm, jsr) = match lock.get("version").and_then(Value::as_str) {
        Some("2") => (lock.pointer("/npm/packages"), None),
        Some("3") => (lock.pointer("/packages/npm"), lock.pointer("/packages/jsr")),
        Some("4" | "5") => (lock.get("npm"), lock.get("jsr")),
        version => {
            debug!(
                ?version,
                path = %path.display(),
                "library store: a deno.lock format this scanner does not read; nothing sent from it"
            );
            return Some(false);
        }
    };
    // A link is keyed with its protocol: `npm:name@1.2.3`.
    let linked = ["/workspace/links", "/workspace/patches"]
        .into_iter()
        .filter_map(|pointer| lock.pointer(pointer).and_then(Value::as_object))
        .flat_map(|links| links.keys())
        .map(|key| key.split_once(':').map_or(key.as_str(), |(_, rest)| rest));
    let elsewhere = jsr
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|jsr| jsr.keys().map(String::as_str))
        .chain(linked)
        .any(|key| deno_lock_name(key) == name);
    if elsewhere {
        return Some(false);
    }
    let entries: Vec<&Value> = npm
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(key, _)| deno_lock_key(key) == Some((name, package.version.as_str())))
        .map(|(_, entry)| entry)
        .collect();
    Some(
        !entries.is_empty()
            && entries.iter().all(|entry| {
                entry
                    .get("integrity")
                    .and_then(Value::as_str)
                    .is_some_and(|integrity| !integrity.is_empty())
                    && entry
                        .get("tarball")
                        .is_none_or(|tarball| tarball.as_str().is_some_and(is_public_url))
            }),
    )
}

/// The package name a `deno.lock` package key names: `@scope/name` of
/// `@scope/name@1.2.3`.
fn deno_lock_name(key: &str) -> &str {
    key.get(1..)
        .and_then(|rest| rest.find('@'))
        .map_or(key, |at| &key[..at + 1])
}

/// The name and version a `deno.lock` package key names: `name@1.2.3`, or
/// with the peer dependencies it resolved against after an `_`
/// (`name@1.2.3_peer@2.0.0`). A version holds no `_`; a name may.
fn deno_lock_key(key: &str) -> Option<(&str, &str)> {
    let name = deno_lock_name(key);
    let version = key.get(name.len() + 1..)?.split('_').next()?;
    Some((name, version))
}

/// What the store answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryAnswer {
    /// Every export of every `answered` package, element by element.
    pub exports: Vec<ExportClaims>,
    /// Every package the store answered, with or without an export.
    pub answered_packages: BTreeSet<String>,
    /// Every package the store answered, with the version it answered.
    pub answered_versions: BTreeSet<(String, String)>,
    /// The packages the store has not finished (`pending`).
    pub pending: BTreeSet<String>,
    /// The part of `pending` a wait can end ([`WAITED_REASONS`]).
    pub building: BTreeSet<String>,
    /// The packages answered `skipped` with `build_failed`: named, never
    /// waited on (carrick#2496).
    pub build_failed: BTreeSet<String>,
    /// Entries answered, for the log.
    pub answered: usize,
    /// Entries skipped, for the log.
    pub skipped: usize,
    /// Elements that failed to parse and were dropped.
    pub dropped: usize,
}

/// The store's answer, read one element at a time (contract section 2): an
/// element that fails to parse is dropped and counted, never the answer. An
/// entry is keyed on its `status` only; its `reason` is logged (A3). An
/// export whose role this build has no word for is dropped.
pub fn parse_answer(text: &str) -> LibraryAnswer {
    let mut answer = LibraryAnswer::default();
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        warn!("library store: the answer is not JSON; no claims this scan");
        return answer;
    };
    let Some(entries) = value.get("library_claims").and_then(Value::as_array) else {
        warn!("library store: the answer carries no library_claims; no claims this scan");
        return answer;
    };
    for entry in entries {
        let status = entry.get("status").and_then(Value::as_str).unwrap_or("");
        let package = entry.get("package").and_then(Value::as_str);
        match status {
            "answered" => {
                answer.answered += 1;
                if let Some(package) = package {
                    answer.answered_packages.insert(package.to_string());
                }
            }
            "pending" => {
                if let Some(package) = package {
                    answer.pending.insert(package.to_string());
                    let reason = entry.get("reason").and_then(Value::as_str);
                    if reason.is_some_and(|reason| WAITED_REASONS.contains(&reason)) {
                        answer.building.insert(package.to_string());
                    }
                }
                continue;
            }
            other => {
                answer.skipped += 1;
                let reason = entry.get("reason").and_then(|reason| reason.as_str());
                if let (Some(package), Some("build_failed")) = (package, reason) {
                    answer.build_failed.insert(package.to_string());
                }
                debug!(
                    package = package.unwrap_or("?"),
                    status = other,
                    reason = reason.unwrap_or(""),
                    "library store: not answered"
                );
                continue;
            }
        }
        let (Some(package), Some(version)) =
            (package, entry.get("version").and_then(Value::as_str))
        else {
            answer.dropped += 1;
            continue;
        };
        answer
            .answered_versions
            .insert((package.to_string(), version.to_string()));
        // Label v2 (carrick#2434): the package's type-only packages. A name
        // that is not a string says nothing and is skipped.
        let type_packages: Vec<String> = entry
            .get("type_packages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|name| {
                name.as_str()
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
            .collect();
        for export in entry
            .get("exports")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match parse_export(
                package,
                version,
                &type_packages,
                export,
                &mut answer.dropped,
            ) {
                Some(claims) => answer.exports.push(claims),
                None => answer.dropped += 1,
            }
        }
    }
    answer
}

/// One export of an answered package, its lists read element by element,
/// each `makes`, `scopes`, `ops` and `reserved` element tagged with its kind.
fn parse_export(
    package: &str,
    version: &str,
    type_packages: &[String],
    export: &Value,
    dropped: &mut usize,
) -> Option<ExportClaims> {
    let specifier = export.get("specifier")?.as_str()?.to_string();
    let name = export.get("export")?.as_str()?.to_string();
    let role: LibraryRole = serde_json::from_value(export.get("role")?.clone()).ok()?;
    let side: Option<Side> = match export.get("side") {
        None | Some(Value::Null) => None,
        Some(side) => Some(serde_json::from_value(side.clone()).ok()?),
    };
    // Label v2 (carrick#2434). A label is carried as written; a side rule
    // this build has no word for drops the export, as an unknown role does.
    let label = match export.get("label") {
        None | Some(Value::Null) => None,
        Some(label) => Some(label.as_str()?.to_string()),
    };
    let side_rule: Option<SideRule> = match export.get("side_rule") {
        None | Some(Value::Null) => None,
        Some(rule) => Some(serde_json::from_value(rule.clone()).ok()?),
    };
    let elements = |list: &str| -> Vec<Value> {
        export
            .get(list)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut claims: Vec<LibraryClaim> = Vec::new();
    for (list, kind) in [
        ("makes", "make"),
        ("scopes", "scope"),
        ("ops", "op"),
        ("reserved", "reserved"),
    ] {
        for mut element in elements(list) {
            let tagged = element.as_object_mut().map(|fields| {
                fields.insert("kind".to_string(), Value::String(kind.to_string()));
            });
            let parsed = tagged
                .and_then(|()| serde_json::from_value::<LibraryClaim>(element).ok())
                // A scope names a prefix or collects procedures (#2474 R6a);
                // one that does neither is out of shape.
                .filter(|claim| {
                    !matches!(
                        claim,
                        LibraryClaim::Scope {
                            name: None,
                            collects: None,
                            ..
                        }
                    )
                });
            match parsed {
                Some(claim) => claims.push(claim),
                None => *dropped += 1,
            }
        }
    }
    let mut calls: Vec<CallClaim> = Vec::new();
    for element in elements("calls") {
        match serde_json::from_value::<CallClaim>(element) {
            Ok(call) => calls.push(call),
            Err(_) => *dropped += 1,
        }
    }
    let patterns: Vec<String> = elements("patterns")
        .iter()
        .filter_map(|pattern| pattern.as_str().map(str::to_string))
        .collect();
    Some(ExportClaims {
        package: package.to_string(),
        version: version.to_string(),
        specifier,
        export: name,
        role,
        label,
        side,
        side_rule,
        type_packages: type_packages.to_vec(),
        claims,
        calls,
        patterns,
    })
}

/// Ask the store once, through `send`. Every failure and a timeout are no
/// claims: logged, and the scan carries on.
///
/// `gate` is awaited before the ask starts and its output held until the ask
/// is over, so services that would send one request side by side can take
/// turns (carrick#1066). The wait is outside [`ASK_TIMEOUT`]: a service
/// queued behind another's ask is not out of time for it.
pub async fn ask<G, GateFut, S, Fut>(
    request: LibraryClaimsRequest,
    gate: G,
    send: S,
) -> LibraryAnswer
where
    G: FnOnce(&LibraryClaimsRequest) -> GateFut,
    GateFut: Future,
    S: FnOnce(LibraryClaimsRequest) -> Fut,
    Fut: Future<Output = Result<String, AgentCallError>>,
{
    try_ask(request, gate, send, ASK_TIMEOUT)
        .await
        .unwrap_or_default()
}

/// [`ask`], saying when there was no answer (a refusal, an error, a timeout
/// of `limit`) instead of reading it as an answer naming nothing.
async fn try_ask<G, GateFut, S, Fut>(
    request: LibraryClaimsRequest,
    gate: G,
    send: S,
    limit: Duration,
) -> Option<LibraryAnswer>
where
    G: FnOnce(&LibraryClaimsRequest) -> GateFut,
    GateFut: Future,
    S: FnOnce(LibraryClaimsRequest) -> Fut,
    Fut: Future<Output = Result<String, AgentCallError>>,
{
    let _turn = gate(&request).await;
    match tokio::time::timeout(limit, send(request)).await {
        Ok(Ok(text)) => Some(parse_answer(&text)),
        Ok(Err(error)) => {
            warn!(%error, "library store: no claims this scan");
            None
        }
        Err(_) => {
            warn!("library store: no answer in time; no claims this scan");
            None
        }
    }
}

/// What the store gave a scan once it stopped asking.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settled {
    /// Every export of every package answered, from any ask.
    pub exports: Vec<ExportClaims>,
    /// The packages the scan ends without: pending at the last answer, or
    /// still being built when the wait ran out or the store stopped answering.
    pub pending: BTreeSet<String>,
    /// The packages the store answered `skipped` with `build_failed`.
    pub build_failed: BTreeSet<String>,
    /// Whether the store was asked at all.
    pub asked: bool,
}

/// The store's first answer to a scan, sent the moment it arrives while the
/// wait for the packages still building carries on (carrick#2613).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FirstAnswer {
    /// Every export of every package answered, as [`Settled::exports`] holds
    /// them: the same filter, so a settle that adds nothing equals it.
    pub exports: Vec<ExportClaims>,
    /// The packages this answer leaves without exports: pending for any
    /// reason, answered `skipped` with `build_failed`, and every package
    /// asked when the ask itself failed.
    pub unanswered: BTreeSet<String>,
    /// Every package the answer holds as answered for this service, with or
    /// without an export ([`Asked::answered`]).
    pub answered: BTreeSet<String>,
    /// The ask itself got no answer (a refusal, an error, a timeout).
    pub ask_failed: bool,
}

/// The store's answer for `asked`, waiting for the packages it is building
/// (carrick#2496). The first ask is the whole request. The packages it
/// answered `pending` for a reason a build can end ([`WAITED_REASONS`]) are
/// asked again, alone, every [`POLL_INTERVAL`] until each is answered or
/// [`WAIT_CAP`] has passed since the first ask began. The wait is this
/// future's alone, so the caller runs it beside the analysis. A store that
/// stops answering ends the wait: it never fails the scan.
///
/// `gate` is taken before every ask, as [`ask`]'s is. `early` receives the
/// first answer as soon as there is one, on every path: the caller that
/// passes one may be waiting on it (carrick#2613).
pub async fn ask_until_settled<G, GateFut, S, Fut>(
    asked: &Asked,
    gate: G,
    send: S,
    early: Option<tokio::sync::oneshot::Sender<FirstAnswer>>,
) -> Settled
where
    G: Fn(&LibraryClaimsRequest) -> GateFut,
    GateFut: Future,
    S: Fn(LibraryClaimsRequest) -> Fut,
    Fut: Future<Output = Result<String, AgentCallError>>,
{
    let started = tokio::time::Instant::now();
    let deadline = started + WAIT_CAP;
    let Some(first) = try_ask(asked.request.clone(), &gate, &send, ASK_TIMEOUT).await else {
        if let Some(early) = early {
            let _ = early.send(FirstAnswer {
                exports: Vec::new(),
                unanswered: asked
                    .request
                    .packages
                    .iter()
                    .map(|package| package.package.clone())
                    .collect(),
                answered: BTreeSet::new(),
                ask_failed: true,
            });
        }
        return Settled {
            asked: true,
            ..Settled::default()
        };
    };
    if let Some(early) = early {
        let _ = early.send(FirstAnswer {
            exports: asked.claims(first.exports.clone()),
            unanswered: first.pending.union(&first.build_failed).cloned().collect(),
            answered: asked.answered(&first),
            ask_failed: false,
        });
    }
    let mut exports = first.exports;
    let mut pending: BTreeSet<String> =
        first.pending.difference(&first.building).cloned().collect();
    let mut building = first.building;
    let mut build_failed = first.build_failed;
    let mut polls = 0usize;
    while !building.is_empty() {
        let next = tokio::time::Instant::now() + POLL_INTERVAL;
        if next >= deadline {
            break;
        }
        tokio::time::sleep_until(next).await;
        let Some(again) = asked.only(&building) else {
            break;
        };
        let limit = ASK_TIMEOUT.min(deadline - tokio::time::Instant::now());
        polls += 1;
        let Some(second) = try_ask(again, &gate, &send, limit).await else {
            break;
        };
        exports.extend(second.exports);
        pending.extend(second.pending.difference(&second.building).cloned());
        build_failed.extend(second.build_failed);
        building = second.building;
    }
    pending.extend(building);
    debug!(
        answered = first.answered,
        skipped = first.skipped,
        dropped = first.dropped,
        polls,
        waited_s = started.elapsed().as_secs(),
        pending = pending.len(),
        "library store: answered"
    );
    Settled {
        exports: asked.claims(exports),
        pending,
        build_failed,
        asked: true,
    }
}

/// The store's own 200 bodies, for the tests on both sides of the
/// transport.
#[cfg(test)]
pub(crate) mod store_bodies {
    /// The store's 200 body for an answered package, byte for byte as
    /// carrick-cloud 9f9bcc1d5e1278fc6578b23a9145310bfb3909c2 sends it: its
    /// `lambdas/library-claims/http.test.js` case "answered: the requested
    /// roles only, trimmed to the imported specifiers", run through
    /// `answerLibraryClaims` and serialised as `index.ts` does
    /// (`JSON.stringify({ library_claims })`).
    pub const ANSWERED: &str = r#"{"library_claims":[{"package":"lib","version":"1.0.0","status":"answered","surface_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","exports":[{"specifier":"lib","export":"default","role":"broker","side":null,"classifier":"jev-test/q1","makes":[],"scopes":[],"ops":[{"op":"send","member":"publish","on":"export","name":{"arg":0},"payload":{"arg":1},"picker":"gemini-test/q1"}],"reserved":[],"calls":[],"patterns":[]}]}]}"#;

    /// The store's 200 body under label structure v2 (carrick#2434), byte
    /// for byte as carrick-cloud's `lambdas/library-claims/http.test.js`
    /// case "label v2 rides on the wire" sends it: a `ver` row's
    /// `type_packages`, a class row's `side_rule` on a picked role and its
    /// `label` on a class-only role.
    pub const ANSWERED_V2: &str = r#"{"library_claims":[{"package":"lib","version":"1.0.0","status":"answered","surface_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","type_packages":["@types/lib"],"exports":[{"specifier":"lib","export":"Server","role":"socket","side":null,"side_rule":"by_origin","classifier":"jev-test/q1","makes":[],"scopes":[],"ops":[{"op":"send","member":"publish","on":"export","name":{"arg":0},"payload":{"arg":1},"picker":"gemini-test/q1"}],"reserved":[],"calls":[],"patterns":[]},{"specifier":"lib","export":"request","role":"http_client","label":"request_runner","side":null,"classifier":"jev-test/q1","makes":[],"scopes":[],"ops":[],"reserved":[],"calls":[],"patterns":[]}]}]}"#;

    /// The same store's 200 body for a package it cannot answer yet: the
    /// case "with no lister pinned, every package is pending and no build
    /// starts".
    pub const PENDING: &str = r#"{"library_claims":[{"package":"lib","version":"1.0.0","status":"pending","reason":"lister_unavailable"}]}"#;
}

#[cfg(test)]
mod tests {
    use super::store_bodies::{
        ANSWERED as STORE_ANSWERED, ANSWERED_V2 as STORE_ANSWERED_V2, PENDING as STORE_PENDING,
    };
    use super::*;
    use std::fs;

    const PUBLIC_TGZ: &str = "https://registry.npmjs.org/x/-/x-1.0.0.tgz";

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// `name@version` installed in `root/node_modules`.
    fn install_package(root: &Path, name: &str, version: &str) {
        write(
            &root.join("node_modules").join(name).join("package.json"),
            &format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        );
    }

    /// A v3 `package-lock.json` in `root` listing each `(name, resolved)` at
    /// `node_modules/<name>`.
    fn npm_lockfile(root: &Path, packages: &[(&str, &str)]) {
        let entries: serde_json::Map<String, Value> = packages
            .iter()
            .map(|(name, resolved)| {
                (
                    format!("node_modules/{name}"),
                    serde_json::json!({"version": "1.0.0", "resolved": resolved}),
                )
            })
            .collect();
        write(
            &root.join("package-lock.json"),
            &serde_json::json!({"lockfileVersion": 3, "packages": entries}).to_string(),
        );
    }

    fn install_at(root: &Path) -> Install<'_> {
        Install {
            service_root: root,
            repo_root: root,
            home: None,
            yarn_registry_env: None,
            npm_registry_env: None,
        }
    }

    fn specifiers(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The packages a request names, with their versions and specifiers.
    fn asked(asked: Option<Asked>) -> Vec<(String, String, Vec<String>)> {
        asked
            .map(|asked| asked.request.packages)
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.package, p.version, p.specifiers))
            .collect()
    }

    fn one(
        package: &str,
        version: &str,
        specifiers: &[&str],
    ) -> Vec<(String, String, Vec<String>)> {
        vec![(
            package.to_string(),
            version.to_string(),
            specifiers.iter().map(|s| s.to_string()).collect(),
        )]
    }

    #[test]
    fn a_public_npm_package_is_sent_with_its_installed_version_and_specifiers() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(dir.path(), &[("lib", PUBLIC_TGZ)]);
        let request = request(
            &specifiers(&["lib", "lib/sub", "./local", "@/alias"]),
            &install_at(dir.path()),
        );
        assert_eq!(asked(request), one("lib", "1.0.0", &["lib", "lib/sub"]));
    }

    #[test]
    fn the_request_names_the_message_roles_only() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(dir.path(), &[("lib", PUBLIC_TGZ)]);
        let request = request(&specifiers(&["lib"]), &install_at(dir.path())).unwrap();
        let wire = serde_json::to_value(&request.request).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "roles": ["broker", "socket", "in_process_bus"],
                "packages": [{"package": "lib", "version": "1.0.0", "specifiers": ["lib"]}],
            }),
            "no workspace_packages: in-repo packages are carrick#1666's"
        );
    }

    /// carrick#2436: with site questions off the request is today's, byte for
    /// byte. On, it asks every network role the store answers for the same
    /// packages, and so does the re-ask of a package still being built.
    #[test]
    fn the_request_asks_every_network_role_only_with_site_questions_on() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(dir.path(), &[("lib", PUBLIC_TGZ)]);
        let asked = request(&specifiers(&["lib"]), &install_at(dir.path())).unwrap();
        let off = asked.clone().with_roles(roles_for(false));
        assert_eq!(off, asked);
        assert_eq!(
            serde_json::to_string(&off.request).unwrap(),
            r#"{"roles":["broker","socket","in_process_bus"],"packages":[{"package":"lib","version":"1.0.0","specifiers":["lib"]}]}"#
        );
        let on = asked.with_roles(roles_for(true));
        assert_eq!(
            serde_json::to_string(&on.request).unwrap(),
            r#"{"roles":["broker","socket","in_process_bus","http_client","graphql_client","server_framework"],"packages":[{"package":"lib","version":"1.0.0","specifiers":["lib"]}]}"#
        );
        let again = on.only(&BTreeSet::from(["lib".to_string()])).unwrap();
        assert_eq!(again.roles, NETWORK_ROLES.to_vec());
    }

    /// carrick#2504: the language's own library is one `lib:<file>` package
    /// per file at the compiler's version, narrowed with `exports`, added to
    /// a service's request even when it asks about no other package. A
    /// request without one is the request it was (`exports` is not written).
    #[test]
    fn the_language_s_library_is_a_package_per_file_narrowed_to_its_globals() {
        let globals: BTreeMap<String, BTreeSet<String>> = BTreeMap::from([
            (
                "lib.dom.d.ts".to_string(),
                BTreeSet::from(["WebSocket".to_string(), "fetch".to_string()]),
            ),
            ("lib.es5.d.ts".to_string(), BTreeSet::new()),
        ]);
        let library = default_library_packages("6.0.2", &globals);
        let asked = Asked::with_default_library(None, library).expect("the library is asked");
        assert_eq!(
            serde_json::to_string(&asked.request.packages).unwrap(),
            r#"[{"package":"lib:lib.dom.d.ts","version":"6.0.2","specifiers":["lib:lib.dom.d.ts"],"exports":["WebSocket","fetch"]}]"#
        );
        assert!(Asked::with_default_library(None, Vec::new()).is_none());
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(dir.path(), &[("lib", PUBLIC_TGZ)]);
        let plain = request(&specifiers(&["lib"]), &install_at(dir.path())).unwrap();
        assert_eq!(
            Asked::with_default_library(Some(plain.clone()), Vec::new()),
            Some(plain)
        );
    }

    #[test]
    fn a_package_resolved_from_a_private_host_is_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(
            dir.path(),
            &[("lib", "https://npm.acme.dev/lib/-/lib-1.0.0.tgz")],
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            vec![]
        );
    }

    #[test]
    fn a_registry_lookalike_host_or_plain_http_is_not_public() {
        assert!(is_public_url(
            "https://registry.npmjs.org/lib/-/lib-1.0.0.tgz"
        ));
        assert!(is_public_url(
            "https://registry.yarnpkg.com/lib/-/lib-1.0.0.tgz"
        ));
        assert!(!is_public_url(
            "http://registry.npmjs.org/lib/-/lib-1.0.0.tgz"
        ));
        assert!(!is_public_url(
            "https://registry.npmjs.org.acme.dev/lib.tgz"
        ));
        assert!(!is_public_url(
            "https://acme.dev/registry.npmjs.org/lib.tgz"
        ));
    }

    #[test]
    fn a_private_scope_mapped_in_npmrc_is_not_sent_though_its_lockfile_says_public() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "@acme/lib", "1.0.0");
        npm_lockfile(dir.path(), &[("@acme/lib", PUBLIC_TGZ)]);
        write(
            &dir.path().join(".npmrc"),
            "@acme:registry=https://npm.acme.dev/\n",
        );
        assert_eq!(
            asked(request(
                &specifiers(&["@acme/lib"]),
                &install_at(dir.path())
            )),
            vec![]
        );
    }

    #[test]
    fn a_default_registry_mapped_in_the_home_npmrc_is_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(dir.path(), &[("lib", PUBLIC_TGZ)]);
        write(
            &home.path().join(".npmrc"),
            "registry=https://npm.acme.dev/\n",
        );
        let install = Install {
            home: Some(home.path()),
            ..install_at(dir.path())
        };
        assert_eq!(asked(request(&specifiers(&["lib"]), &install)), vec![]);
    }

    #[test]
    fn a_git_dependency_is_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(
            dir.path(),
            &[("lib", "git+ssh://git@github.com/acme/lib.git#abc123")],
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            vec![]
        );
    }

    #[test]
    fn a_linked_workspace_package_is_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        write(
            &dir.path().join("package-lock.json"),
            r#"{"lockfileVersion":3,"packages":{"node_modules/lib":{"resolved":"packages/lib","link":true}}}"#,
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            vec![]
        );
    }

    #[test]
    fn no_lockfile_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        assert!(request(&specifiers(&["lib"]), &install_at(dir.path())).is_none());
    }

    #[test]
    fn a_package_the_lockfile_does_not_list_is_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(dir.path(), &[("other", PUBLIC_TGZ)]);
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            vec![]
        );
    }

    #[test]
    fn a_v1_lockfile_is_read_by_name() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        write(
            &dir.path().join("package-lock.json"),
            &format!(
                r#"{{"lockfileVersion":1,"dependencies":{{"lib":{{"version":"1.0.0","resolved":"{PUBLIC_TGZ}"}}}}}}"#
            ),
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            one("lib", "1.0.0", &["lib"])
        );
    }

    #[test]
    fn a_monorepo_service_reads_the_root_lockfile_at_the_package_s_path() {
        let dir = tempfile::tempdir().unwrap();
        let service = dir.path().join("packages").join("api");
        install_package(&service, "lib", "2.0.0");
        write(
            &dir.path().join("package-lock.json"),
            &format!(
                r#"{{"lockfileVersion":3,"packages":{{"packages/api/node_modules/lib":{{"version":"2.0.0","resolved":"{PUBLIC_TGZ}"}}}}}}"#
            ),
        );
        let install = Install {
            service_root: &service,
            repo_root: dir.path(),
            home: None,
            yarn_registry_env: None,
            npm_registry_env: None,
        };
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install)),
            one("lib", "2.0.0", &["lib"])
        );
    }

    #[test]
    fn two_lockfiles_that_disagree_send_nothing() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        npm_lockfile(dir.path(), &[("lib", PUBLIC_TGZ)]);
        write(
            &dir.path().join("yarn.lock"),
            "lib@^1.0.0:\n  version \"1.0.0\"\n  resolved \"https://npm.acme.dev/lib/-/lib-1.0.0.tgz\"\n",
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            vec![]
        );
    }

    /// A pnpm install: `.modules.yaml` records the registries, the lockfile
    /// the resolution.
    fn pnpm_install(root: &Path, registries: &str, resolution: &str) {
        install_package(root, "lib", "1.0.0");
        write(
            &root.join("node_modules").join(".modules.yaml"),
            &format!("layoutVersion: 5\nregistries:\n{registries}"),
        );
        write(
            &root.join("pnpm-lock.yaml"),
            &format!(
                "lockfileVersion: '9.0'\npackages:\n  lib@1.0.0:\n    resolution: {resolution}\n"
            ),
        );
    }

    #[test]
    fn a_pnpm_package_from_the_public_registry_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        pnpm_install(
            dir.path(),
            "  default: https://registry.npmjs.org/\n",
            "{integrity: sha512-abc}",
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            one("lib", "1.0.0", &["lib"])
        );
    }

    /// pnpm 9.10.0 with `lockfile-include-tarball-url=true` writes every
    /// entry this way, public ones included (carrick#1721).
    #[test]
    fn a_pnpm_package_whose_tarball_is_on_the_public_registry_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        pnpm_install(
            dir.path(),
            "  default: https://registry.npmjs.org/\n",
            "{integrity: sha512-abc, tarball: https://registry.npmjs.org/lib/-/lib-1.0.0.tgz}",
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            one("lib", "1.0.0", &["lib"])
        );
    }

    #[test]
    fn a_pnpm_tarball_off_the_public_registry_or_private_default_registry_is_not_sent() {
        for url in [
            "https://npm.acme.dev/lib.tgz",
            "https://npm.acme.dev/lib/-/lib-1.0.0.tgz",
            "http://registry.npmjs.org/lib/-/lib-1.0.0.tgz",
        ] {
            let tarball = tempfile::tempdir().unwrap();
            pnpm_install(
                tarball.path(),
                "  default: https://registry.npmjs.org/\n",
                &format!("{{integrity: sha512-abc, tarball: {url}}}"),
            );
            assert_eq!(
                asked(request(&specifiers(&["lib"]), &install_at(tarball.path()))),
                vec![],
                "{url}"
            );
        }
        let private = tempfile::tempdir().unwrap();
        pnpm_install(
            private.path(),
            "  default: https://npm.acme.dev/\n",
            "{integrity: sha512-abc}",
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(private.path()))),
            vec![]
        );
    }

    #[test]
    fn yarn_classic_reads_the_resolved_url_of_the_installed_version() {
        let public = tempfile::tempdir().unwrap();
        install_package(public.path(), "lib", "1.0.0");
        write(
            &public.path().join("yarn.lock"),
            &format!(
                "# yarn lockfile v1\n\n\"lib@^0.9.0\":\n  version \"0.9.0\"\n  resolved \"https://npm.acme.dev/lib.tgz\"\n\nlib@^1.0.0, \"lib@~1.0.0\":\n  version \"1.0.0\"\n  resolved \"{PUBLIC_TGZ}\"\n"
            ),
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(public.path()))),
            one("lib", "1.0.0", &["lib"])
        );
        // The installed version's block is the one read: another version's
        // public URL says nothing about it.
        let private = tempfile::tempdir().unwrap();
        install_package(private.path(), "lib", "1.0.0");
        write(
            &private.path().join("yarn.lock"),
            &format!(
                "lib@^0.9.0:\n  version \"0.9.0\"\n  resolved \"{PUBLIC_TGZ}\"\n\nlib@^1.0.0:\n  version \"1.0.0\"\n  resolved \"https://npm.acme.dev/lib.tgz\"\n"
            ),
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(private.path()))),
            vec![]
        );
    }

    /// A Berry install of `name@1.0.0`, resolved by the `npm:` protocol.
    fn berry_install(root: &Path, name: &str) {
        install_package(root, name, "1.0.0");
        write(
            &root.join("yarn.lock"),
            &format!(
                "__metadata:\n  version: 8\n\n\"{name}@npm:^1.0.0\":\n  version: 1.0.0\n  resolution: \"{name}@npm:1.0.0\"\n"
            ),
        );
    }

    #[test]
    fn yarn_berry_with_no_registry_configured_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        berry_install(dir.path(), "lib");
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            one("lib", "1.0.0", &["lib"])
        );
    }

    #[test]
    fn yarn_berry_reads_the_registry_from_every_config_it_could_use() {
        // A scope mapped only in the home `.yarnrc.yml`.
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        berry_install(dir.path(), "@acme/lib");
        write(
            &home.path().join(".yarnrc.yml"),
            "npmScopes:\n  acme:\n    npmRegistryServer: \"https://npm.acme.dev\"\n",
        );
        let install = Install {
            home: Some(home.path()),
            ..install_at(dir.path())
        };
        assert_eq!(
            asked(request(&specifiers(&["@acme/lib"]), &install)),
            vec![]
        );
        // A default registry in the project's own `.yarnrc.yml`.
        let project = tempfile::tempdir().unwrap();
        berry_install(project.path(), "lib");
        write(
            &project.path().join(".yarnrc.yml"),
            "npmRegistryServer: \"https://npm.acme.dev\"\n",
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(project.path()))),
            vec![]
        );
        // The environment.
        let env = tempfile::tempdir().unwrap();
        berry_install(env.path(), "lib");
        let install = Install {
            yarn_registry_env: Some("https://npm.acme.dev"),
            ..install_at(env.path())
        };
        assert_eq!(asked(request(&specifiers(&["lib"]), &install)), vec![]);
    }

    #[test]
    fn yarn_berry_never_sends_a_package_resolved_by_another_protocol() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "1.0.0");
        write(
            &dir.path().join("yarn.lock"),
            "__metadata:\n  version: 8\n\n\"lib@workspace:packages/lib\":\n  version: 1.0.0\n  resolution: \"lib@workspace:packages/lib\"\n",
        );
        assert_eq!(
            asked(request(&specifiers(&["lib"]), &install_at(dir.path()))),
            vec![]
        );
    }

    /// The Deno lockfiles of `tests/fixtures/deno-lock` (carrick#1720): each
    /// lists `chalk` 5.3.0 from npm, and from format 3 `@std/path` from JSR.
    const DENO_V2: &str = include_str!("../tests/fixtures/deno-lock/v2.lock");
    const DENO_V3: &str = include_str!("../tests/fixtures/deno-lock/v3.lock");
    const DENO_V4: &str = include_str!("../tests/fixtures/deno-lock/v4.lock");
    const DENO_V5: &str = include_str!("../tests/fixtures/deno-lock/v5.lock");
    const DENO_V5_OTHER_REGISTRY: &str =
        include_str!("../tests/fixtures/deno-lock/v5-other-registry.lock");

    /// A Deno install in `root`: `chalk` at `version`, and `lock` as its
    /// `deno.lock`.
    fn deno_install(root: &Path, version: &str, lock: &str) {
        install_package(root, "chalk", version);
        write(&root.join("deno.lock"), lock);
    }

    /// What a service importing `imports` asks, with `chalk` 5.3.0 installed
    /// by Deno and `lock` as its `deno.lock`.
    fn deno_asked(lock: &str, imports: &[&str]) -> Vec<(String, String, Vec<String>)> {
        let dir = tempfile::tempdir().unwrap();
        deno_install(dir.path(), "5.3.0", lock);
        asked(request(&specifiers(imports), &install_at(dir.path())))
    }

    /// `lock` with `entry` added to its npm section under `key`.
    fn with_npm_entry(lock: &str, pointer: &str, key: &str, entry: Value) -> String {
        let mut lock: Value = serde_json::from_str(lock).unwrap();
        lock.pointer_mut(pointer)
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert(key.to_string(), entry);
        lock.to_string()
    }

    #[test]
    fn a_deno_install_sends_its_public_npm_packages_in_every_lockfile_format() {
        for (format, lock) in [
            ("2", DENO_V2),
            ("3", DENO_V3),
            ("4", DENO_V4),
            ("5", DENO_V5),
        ] {
            assert_eq!(
                deno_asked(lock, &["chalk"]),
                one("chalk", "5.3.0", &["chalk"]),
                "format {format}"
            );
        }
    }

    #[test]
    fn a_deno_lockfile_this_scanner_cannot_read_sends_nothing() {
        // Format 1 has no version field and records remote modules only.
        let format_1 = r#"{"https://deno.land/std@0.71.0/textproto/mod.ts":"3118d7a42c03c242c5a49c2ad91c8396110e14acca1324e7aaefd31a999b71a4"}"#;
        // A format after 5, laid out as 5 is.
        let format_6 = DENO_V5.replace(r#""version": "5""#, r#""version": "6""#);
        assert_ne!(format_6, DENO_V5);
        for (case, lock) in [
            ("format 1", format_1),
            ("format 6", format_6.as_str()),
            ("not JSON", "{\"version\": \"5\","),
        ] {
            assert_eq!(deno_asked(lock, &["chalk"]), vec![], "{case}");
        }
    }

    #[test]
    fn a_deno_package_fetched_from_another_registry_is_not_sent() {
        // Format 5 records the other host as the package's tarball.
        assert_eq!(deno_asked(DENO_V5_OTHER_REGISTRY, &["chalk"]), vec![]);
        let public_tarball = with_npm_entry(
            DENO_V5,
            "/npm",
            "chalk@5.3.0",
            serde_json::json!({
                "integrity": "sha512-x",
                "tarball": "https://registry.npmjs.org/chalk/-/chalk-5.3.0.tgz",
            }),
        );
        assert_eq!(
            deno_asked(&public_tarball, &["chalk"]),
            one("chalk", "5.3.0", &["chalk"])
        );
        // Formats 2 to 4 record no host: the registry Deno was set to is read.
        let dir = tempfile::tempdir().unwrap();
        deno_install(dir.path(), "5.3.0", DENO_V4);
        let with_env = |registry| Install {
            npm_registry_env: Some(registry),
            ..install_at(dir.path())
        };
        assert_eq!(
            asked(request(
                &specifiers(&["chalk"]),
                &with_env("https://npm.internal.example/")
            )),
            vec![]
        );
        assert_eq!(
            asked(request(
                &specifiers(&["chalk"]),
                &with_env("https://registry.npmjs.org/")
            )),
            one("chalk", "5.3.0", &["chalk"])
        );
        write(
            &dir.path().join(".npmrc"),
            "registry=https://npm.internal.example/\n",
        );
        assert_eq!(
            asked(request(&specifiers(&["chalk"]), &install_at(dir.path()))),
            vec![]
        );
    }

    #[test]
    fn a_deno_entry_without_its_integrity_or_at_another_version_is_not_sent() {
        for entry in [serde_json::json!({}), serde_json::json!({"integrity": ""})] {
            let lock = with_npm_entry(DENO_V5, "/npm", "chalk@5.3.0", entry.clone());
            assert_eq!(deno_asked(&lock, &["chalk"]), vec![], "{entry}");
        }
        let dir = tempfile::tempdir().unwrap();
        deno_install(dir.path(), "5.3.1", DENO_V5);
        assert_eq!(
            asked(request(&specifiers(&["chalk"]), &install_at(dir.path()))),
            vec![]
        );
    }

    #[test]
    fn a_deno_key_is_read_past_its_peer_suffix_and_through_an_underscored_name() {
        assert_eq!(
            deno_lock_key("string_decoder@1.3.0"),
            Some(("string_decoder", "1.3.0"))
        );
        assert_eq!(
            deno_lock_key("@babel/plugin-syntax-jsx@7.28.6_@babel+core@7.29.0"),
            Some(("@babel/plugin-syntax-jsx", "7.28.6"))
        );
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "@acme/plugin", "1.0.0");
        let lock = with_npm_entry(
            DENO_V5,
            "/npm",
            "@acme/plugin@1.0.0_@acme+core@2.0.0",
            serde_json::json!({"integrity": "sha512-x"}),
        );
        write(&dir.path().join("deno.lock"), &lock);
        let sent = || {
            asked(request(
                &specifiers(&["@acme/plugin"]),
                &install_at(dir.path()),
            ))
        };
        assert_eq!(sent(), one("@acme/plugin", "1.0.0", &["@acme/plugin"]));
        // Every entry of the installed version must be public.
        write(
            &dir.path().join("deno.lock"),
            &with_npm_entry(
                &lock,
                "/npm",
                "@acme/plugin@1.0.0_@acme+core@3.0.0",
                serde_json::json!({
                    "integrity": "sha512-x",
                    "tarball": "https://npm.internal.example/@acme/plugin/-/plugin-1.0.0.tgz",
                }),
            ),
        );
        assert_eq!(sent(), vec![]);
    }

    #[test]
    fn a_deno_name_the_lockfile_also_takes_from_jsr_or_a_linked_folder_is_not_sent() {
        // `@std/path` also in the npm section: a bare import of it may still
        // resolve to JSR.
        for (format, lock, npm, jsr_parent) in [
            ("3", DENO_V3, "/packages/npm", "/packages"),
            ("5", DENO_V5, "/npm", ""),
        ] {
            let lock = with_npm_entry(
                lock,
                npm,
                "@std/path@1.1.6",
                serde_json::json!({"integrity": "sha512-x"}),
            );
            let dir = tempfile::tempdir().unwrap();
            install_package(dir.path(), "@std/path", "1.1.6");
            write(&dir.path().join("deno.lock"), &lock);
            assert_eq!(
                asked(request(
                    &specifiers(&["@std/path"]),
                    &install_at(dir.path())
                )),
                vec![],
                "format {format}"
            );
            // The npm entry alone would be sent.
            let mut without_jsr: Value = serde_json::from_str(&lock).unwrap();
            without_jsr
                .pointer_mut(jsr_parent)
                .and_then(Value::as_object_mut)
                .and_then(|parent| parent.remove("jsr"))
                .unwrap();
            write(&dir.path().join("deno.lock"), &without_jsr.to_string());
            assert_eq!(
                asked(request(
                    &specifiers(&["@std/path"]),
                    &install_at(dir.path())
                )),
                one("@std/path", "1.1.6", &["@std/path"]),
                "format {format} without JSR"
            );
        }
        // A link, under either name the workspace section has used.
        for links in ["links", "patches"] {
            let mut lock: Value = serde_json::from_str(DENO_V5).unwrap();
            lock["workspace"][links] = serde_json::json!({"npm:chalk@5.3.0": {}});
            assert_eq!(deno_asked(&lock.to_string(), &["chalk"]), vec![], "{links}");
        }
    }

    #[test]
    fn a_jsr_or_url_import_is_never_sent() {
        assert_eq!(
            deno_asked(
                DENO_V5,
                &[
                    "jsr:@std/path@1",
                    "@std/path",
                    "https://deno.land/x/lib/mod.ts",
                    "chalk"
                ]
            ),
            one("chalk", "5.3.0", &["chalk"])
        );
    }

    /// Deno installs each package under `node_modules/.deno` and links the
    /// ones the service imports into `node_modules`.
    #[cfg(unix)]
    #[test]
    fn a_deno_node_modules_is_read_through_its_links() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path()
                .join("node_modules/.deno/chalk@5.3.0/node_modules/chalk/package.json"),
            r#"{"name":"chalk","version":"5.3.0"}"#,
        );
        std::os::unix::fs::symlink(
            ".deno/chalk@5.3.0/node_modules/chalk",
            dir.path().join("node_modules/chalk"),
        )
        .unwrap();
        write(&dir.path().join("deno.lock"), DENO_V5);
        assert_eq!(
            asked(request(&specifiers(&["chalk"]), &install_at(dir.path()))),
            one("chalk", "5.3.0", &["chalk"])
        );
    }

    /// `@types/node` at 22.5.0, declaring the `events` module and the
    /// `fs/promises` subpath, as the real package does.
    fn install_runtime_types(root: &Path) {
        install_package(root, "@types/node", "22.5.0");
        write(
            &root.join("node_modules/@types/node/events.d.ts"),
            "declare module \"events\" {}\n",
        );
        write(
            &root.join("node_modules/@types/node/fs/promises.d.ts"),
            "declare module \"fs/promises\" {}\n",
        );
    }

    #[test]
    fn a_runtime_module_is_asked_in_its_node_form_at_the_runtime_types_version() {
        let dir = tempfile::tempdir().unwrap();
        install_runtime_types(dir.path());
        let request = request(
            &specifiers(&["events", "node:events", "node:fs/promises", "fs/promises"]),
            &install_at(dir.path()),
        );
        assert_eq!(
            asked(request),
            one("node:events", "22.5.0", &["node:events"])
        );
    }

    #[test]
    fn a_runtime_module_s_answer_reaches_every_spelling_the_service_imports() {
        let dir = tempfile::tempdir().unwrap();
        install_runtime_types(dir.path());
        let asked = request(
            &specifiers(&["events", "node:events"]),
            &install_at(dir.path()),
        )
        .unwrap();
        let answered = parse_answer(
            r#"{"library_claims":[{"package":"node:events","version":"22.5.0","status":"answered","exports":[
                {"specifier":"node:events","export":"EventEmitter","role":"in_process_bus","side":null,"ops":[]}]}]}"#,
        );
        let mut spellings: Vec<String> = asked
            .claims(answered.exports)
            .into_iter()
            .map(|export| export.specifier)
            .collect();
        spellings.sort();
        assert_eq!(spellings, vec!["events", "node:events"]);
    }

    #[test]
    fn a_bare_name_the_service_declares_is_the_registry_package() {
        let dir = tempfile::tempdir().unwrap();
        install_runtime_types(dir.path());
        write(
            &dir.path().join("package.json"),
            r#"{"dependencies":{"events":"^3.3.0"}}"#,
        );
        install_package(dir.path(), "events", "3.3.0");
        npm_lockfile(
            dir.path(),
            &[("events", PUBLIC_TGZ), ("@types/node", PUBLIC_TGZ)],
        );
        assert_eq!(
            asked(request(&specifiers(&["events"]), &install_at(dir.path()))),
            one("events", "3.3.0", &["events"])
        );
    }

    #[test]
    fn a_runtime_module_with_no_runtime_types_installed_is_not_asked() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            request(
                &specifiers(&["node:events", "events"]),
                &install_at(dir.path())
            )
            .is_none()
        );
    }

    #[test]
    fn the_request_stays_within_the_store_s_caps() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..MAX_PACKAGES + 1)
            .map(|i| format!("lib{i:03}"))
            .collect();
        let mut wanted: Vec<String> = Vec::new();
        for name in &names {
            install_package(dir.path(), name, "1.0.0");
            wanted.push(name.clone());
        }
        wanted.extend((0..MAX_SPECIFIERS + 1).map(|i| format!("lib000/sub{i:02}")));
        let locked: Vec<(&str, &str)> = names
            .iter()
            .map(|name| (name.as_str(), PUBLIC_TGZ))
            .collect();
        npm_lockfile(dir.path(), &locked);
        let request = request(&wanted.into_iter().collect(), &install_at(dir.path())).unwrap();
        assert_eq!(request.request.packages.len(), MAX_PACKAGES);
        assert_eq!(request.request.packages[0].specifiers.len(), MAX_SPECIFIERS);
    }

    /// carrick#2618: a service's ask takes the packages its siblings ask.
    /// Its own come first, a package at one version is asked once with every
    /// specifier, and the cap drops a sibling's entries, counted.
    #[test]
    fn a_siblings_packages_join_the_ask_after_the_services_own() {
        let own = Asked::naming(&["a", "b"]);
        let mut sibling = Asked::naming(&["b", "c"]);
        sibling.request.packages[0].specifiers = vec!["b/sub".to_string()];
        let (merged, dropped) = Asked::merged(Some(own.clone()), [sibling.clone()]);
        let merged = merged.unwrap();
        let names: Vec<&str> = merged
            .request
            .packages
            .iter()
            .map(|package| package.package.as_str())
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(dropped, 0);
        assert_eq!(merged.request.packages[1].specifiers, ["b", "b/sub"]);
        // A service that asks nothing takes the sibling's whole ask.
        let (alone, _) = Asked::merged(None, [sibling.clone()]);
        assert_eq!(alone.unwrap().request.packages.len(), 2);
        // No siblings: the ask is as it was.
        assert_eq!(Asked::merged(Some(own.clone()), []).0, Some(own));
        // The cap keeps the service's own and drops the sibling's extra.
        let names: Vec<String> = (0..MAX_PACKAGES).map(|i| format!("p{i:03}")).collect();
        let full = Asked::naming(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let (capped, dropped) = Asked::merged(Some(full), [Asked::naming(&["extra"])]);
        assert_eq!(capped.unwrap().request.packages.len(), MAX_PACKAGES);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn the_store_s_answered_body_reads_into_claims() {
        let answer = parse_answer(STORE_ANSWERED);
        assert_eq!((answer.answered, answer.dropped), (1, 0));
        assert_eq!(answer.exports.len(), 1);
        let export = &answer.exports[0];
        assert_eq!(
            (
                export.package.as_str(),
                export.version.as_str(),
                export.specifier.as_str(),
                export.export.as_str()
            ),
            ("lib", "1.0.0", "lib", "default")
        );
        assert_eq!(export.role, LibraryRole::Broker);
        assert!(matches!(
            &export.claims[..],
            [LibraryClaim::Op { member: Some(member), payload: Some(_), .. }] if member == "publish"
        ));
    }

    #[test]
    fn the_store_s_pending_body_names_the_package_to_ask_again() {
        let answer = parse_answer(STORE_PENDING);
        assert_eq!(answer.pending, specifiers(&["lib"]));
        assert!(answer.exports.is_empty());
    }

    #[test]
    fn a_malformed_element_is_dropped_beside_the_good_ones() {
        let answer = parse_answer(
            r#"{"library_claims":[
                {"package":null,"version":null,"status":"skipped","reason":"invalid_package"},
                {"package":"lib","version":"1.0.0","status":"answered","exports":[
                    {"specifier":"lib","export":"default","role":"broker","side":null,"ops":[
                        {"op":"send","member":"publish","on":"export","name":{"arg":0}},
                        {"op":"teleport","member":"publish","on":"export"},
                        7
                    ],"calls":[{"member":"quit","on":"export","kind":"off_wire"},{"kind":"sideways"}]},
                    {"specifier":"lib","export":"other","role":"a_role_from_the_future"},
                    {"specifier":"lib","export":"io","role":"socket","side":"sideways"}
                ]},
                "not an entry"
            ]}"#,
        );
        assert_eq!(answer.exports.len(), 1);
        assert_eq!(answer.exports[0].claims.len(), 1);
        assert_eq!(answer.exports[0].calls.len(), 1);
        // Two ops, one call and two exports.
        assert_eq!(answer.dropped, 5);
        assert_eq!(answer.skipped, 2);
    }

    /// Label structure v2 (carrick#2434): every field the rebuilt store
    /// writes reads into the claims, and the checks the scanner sends carry
    /// it on to the verifier.
    #[test]
    fn a_label_v2_answer_reads_every_field() {
        let answer = parse_answer(
            r#"{"library_claims":[{"package":"fixture-sockets","version":"2.0.0","status":"answered",
                "type_packages":["@types/fixture-sockets","",7],"exports":[
                {"specifier":"fixture-sockets","export":"SocketServer","role":"socket","label":"socket_server",
                 "side":null,"side_rule":"by_origin",
                 "makes":[{"form":"new","member":null,"runs":{"arg":0,"key":"handler"}}],
                 "ops":[
                    {"op":"receive","member":"on","of":"instance:new","handler":{"arg":1},
                     "yields":{"param":0,"export":"Socket","side":"server"}},
                    {"op":"request","member":"request","of":"instance:new","name":{"arg":0},
                     "method_key":{"arg":1,"key":"method"},"method_default":"GET","base":{"arg":2}},
                    {"op":"request","member":"mutate","of":"instance:new","runs":{"bound":"maker","arg":0}}
                 ]}]}]}"#,
        );
        assert_eq!((answer.answered, answer.dropped), (1, 0));
        let export = &answer.exports[0];
        assert_eq!(export.label.as_deref(), Some("socket_server"));
        assert_eq!(export.side_rule, Some(SideRule::ByOrigin));
        assert_eq!(
            export.type_packages,
            vec!["@types/fixture-sockets".to_string()]
        );
        assert_eq!(export.claims.len(), 4);
        let sent = serde_json::to_value(&export.claims).unwrap();
        assert_eq!(
            sent[0]["runs"],
            serde_json::json!({ "arg": 0, "key": "handler" })
        );
        assert_eq!(
            sent[1]["yields"],
            serde_json::json!({ "param": 0, "export": "Socket", "side": "server" })
        );
        assert_eq!(sent[2]["method_default"], "GET");
        assert_eq!(sent[2]["base"], serde_json::json!({ "arg": 2 }));
        assert_eq!(
            sent[3]["runs"],
            serde_json::json!({ "bound": "maker", "arg": 0 })
        );
    }

    #[test]
    fn the_store_s_label_v2_body_reads_its_label_side_rule_and_type_packages() {
        let answer = parse_answer(STORE_ANSWERED_V2);
        assert_eq!((answer.answered, answer.dropped), (1, 0));
        type Read<'a> = (&'a str, Option<&'a str>, Option<SideRule>, &'a [String]);
        let read: Vec<Read> = answer
            .exports
            .iter()
            .map(|export| {
                (
                    export.export.as_str(),
                    export.label.as_deref(),
                    export.side_rule,
                    export.type_packages.as_slice(),
                )
            })
            .collect();
        let types = ["@types/lib".to_string()];
        assert_eq!(
            read,
            vec![
                ("Server", None, Some(SideRule::ByOrigin), &types[..]),
                ("request", Some("request_runner"), None, &types[..]),
            ]
        );
    }

    /// A v2 field in a shape this build cannot read drops its element and
    /// counts it; a side rule it has no word for drops the export.
    #[test]
    fn a_label_v2_field_out_of_shape_is_dropped_and_counted() {
        let answer = parse_answer(
            r#"{"library_claims":[{"package":"lib","version":"1.0.0","status":"answered","exports":[
                {"specifier":"lib","export":"default","role":"http_client","label":"request_runner","ops":[
                    {"op":"request","member":"get","on":"export","name":{"arg":0},"method":"GET"},
                    {"op":"request","member":"get","on":"export","name":{"arg":0},"method_default":5},
                    {"op":"request","member":"get","on":"export","runs":{"bound":"scope","arg":0}},
                    {"op":"receive","member":"on","on":"export","yields":{"param":-1,"export":"S","side":"server"}}
                ]},
                {"specifier":"lib","export":"Socket","role":"socket","side_rule":"by_mood"},
                {"specifier":"lib","export":"Labelled","role":"socket","label":7}
            ]}]}"#,
        );
        assert_eq!(answer.exports.len(), 1);
        assert_eq!(answer.exports[0].label.as_deref(), Some("request_runner"));
        assert_eq!(answer.exports[0].claims.len(), 1);
        // Three ops and two exports.
        assert_eq!(answer.dropped, 5);
    }

    /// carrick#2474 R0: a scope's `runs`, `yields` and `mounts` and a
    /// receive op's `request` read off the store's answer and go to the
    /// verifier as written (contract amendment of 2026-10-22 on #2434).
    #[test]
    fn the_route_fields_read_and_are_sent_as_written() {
        let answer = parse_answer(
            r#"{"library_claims":[{"package":"fixture-server","version":"1.0.0","status":"answered","exports":[
                {"specifier":"fixture-server","export":"default","role":"server_framework",
                 "makes":[{"form":"call","member":null}],
                 "scopes":[
                    {"member":"mount","of":"instance:()","name":{"arg":0},"mounts":{"arg":1}},
                    {"member":"register","of":"instance:()","name":{"arg":1,"key":"prefix"},
                     "runs":{"arg":0},"yields":{"param":0,"export":"App","side":"server"}}
                 ],
                 "ops":[
                    {"op":"receive","member":"get","of":"instance:()","name":{"arg":0},"handler":{"arg":1},
                     "method":"GET","request":{"param":0,"path":["path"],"url":["raw","url"],"method":["method"]}}
                 ]}]}]}"#,
        );
        assert_eq!((answer.answered, answer.dropped), (1, 0));
        let sent = serde_json::to_value(&answer.exports[0].claims).unwrap();
        assert_eq!(sent[1]["mounts"], serde_json::json!({ "arg": 1 }));
        assert_eq!(sent[2]["runs"], serde_json::json!({ "arg": 0 }));
        assert_eq!(
            sent[2]["yields"],
            serde_json::json!({ "param": 0, "export": "App", "side": "server" })
        );
        assert_eq!(
            sent[3]["request"],
            serde_json::json!({ "param": 0, "path": ["path"], "url": ["raw", "url"], "method": ["method"] })
        );
    }

    /// A route field in a shape this build cannot read drops its element and
    /// counts it, as every other field does.
    #[test]
    fn a_route_field_out_of_shape_is_dropped_and_counted() {
        let answer = parse_answer(
            r#"{"library_claims":[{"package":"lib","version":"1.0.0","status":"answered","exports":[
                {"specifier":"lib","export":"default","role":"server_framework",
                 "scopes":[
                    {"member":"mount","on":"instance","name":{"arg":0},"mounts":{"arg":1}},
                    {"member":"mount","on":"instance","name":{"arg":0},"mounts":"child"},
                    {"member":"register","on":"instance","name":{"arg":1},"runs":{"arg":0},
                     "yields":{"param":0,"export":"App"}}
                 ],
                 "ops":[
                    {"op":"receive","member":"get","on":"instance","name":{"arg":0},"handler":{"arg":1},
                     "request":{"param":-1,"path":["path"]}},
                    {"op":"receive","member":"get","on":"instance","name":{"arg":0},"handler":{"arg":1},
                     "request":{"param":0,"path":"path"}}
                 ]}]}]}"#,
        );
        assert_eq!(answer.exports[0].claims.len(), 1);
        assert_eq!(answer.dropped, 4);
    }

    /// carrick#2474 R6a: a router member's `collects`, with no name, an op
    /// named by its key, and an adapter's `runs` and `prefix` read off the
    /// store's answer and go to the verifier as written (contract amendment
    /// 3 of 2026-10-23 on #2434).
    #[test]
    fn the_procedure_fields_read_and_are_sent_as_written() {
        let answer = parse_answer(
            r#"{"library_claims":[{"package":"fixture-procs","version":"1.0.0","status":"answered","exports":[
                {"specifier":"fixture-procs","export":"init","role":"server_framework",
                 "makes":[{"form":"call","member":"create"}],
                 "scopes":[
                    {"member":"router","of":"instance:create","collects":{"arg":0,"join":"."}}
                 ],
                 "ops":[
                    {"op":"receive","member":"query","path":["procedure"],"of":"instance:create",
                     "name":{"bound":"key"},"handler":{"arg":0},"method":"GET"}
                 ]},
                {"specifier":"fixture-procs","export":"serveRpc","role":"server_framework",
                 "makes":[{"form":"call","member":null,"runs":{"arg":0,"key":"router"},
                           "prefix":{"arg":0,"key":"endpoint"}}]}]}]}"#,
        );
        assert_eq!((answer.answered, answer.dropped), (1, 0));
        let sent = serde_json::to_value(&answer.exports[0].claims).unwrap();
        assert_eq!(
            sent[1],
            serde_json::json!({ "kind": "scope", "member": "router", "of": "instance:create",
                                "collects": { "arg": 0, "join": "." } })
        );
        assert_eq!(sent[2]["name"], serde_json::json!({ "bound": "key" }));
        assert_eq!(sent[2]["method"], serde_json::json!("GET"));
        let adapter = serde_json::to_value(&answer.exports[1].claims).unwrap();
        assert_eq!(
            adapter[0],
            serde_json::json!({ "kind": "make", "form": "call", "member": null,
                                "prefix": { "arg": 0, "key": "endpoint" },
                                "runs": { "arg": 0, "key": "router" } })
        );
    }

    /// A procedure field in a shape this build cannot read drops its element
    /// and counts it, as every other field does; so does a scope with neither
    /// a name nor a collection, as one without a name did before.
    #[test]
    fn a_procedure_field_out_of_shape_is_dropped_and_counted() {
        let answer = parse_answer(
            r#"{"library_claims":[{"package":"lib","version":"1.0.0","status":"answered","exports":[
                {"specifier":"lib","export":"init","role":"server_framework",
                 "scopes":[
                    {"member":"router","on":"instance","collects":{"arg":0,"join":"."}},
                    {"member":"router","on":"instance","collects":{"arg":0}},
                    {"member":"router","on":"instance","collects":{"arg":-1,"join":"."}},
                    {"member":"router","on":"instance","collects":"record"},
                    {"member":"router","on":"instance"}
                 ],
                 "ops":[
                    {"op":"receive","member":"query","on":"instance","name":{"bound":"keys"},
                     "handler":{"arg":0},"method":"GET"}
                 ]}]}]}"#,
        );
        assert_eq!(answer.exports[0].claims.len(), 1);
        assert_eq!(answer.dropped, 5);
    }

    /// An answer in the shape before label v2 reads as it did, and its
    /// claims go to the verifier byte for byte as before: no v2 key appears.
    #[test]
    fn an_answer_in_today_s_shape_reads_and_sends_unchanged() {
        let answer = parse_answer(STORE_ANSWERED);
        let export = &answer.exports[0];
        assert_eq!((export.label.as_ref(), export.side_rule), (None, None));
        assert!(export.type_packages.is_empty());
        assert_eq!(
            serde_json::to_string(&export.claims).unwrap(),
            r#"[{"kind":"op","op":"send","member":"publish","on":"export","name":{"arg":0},"payload":{"arg":1},"picker":"gemini-test/q1"}]"#
        );
        let carried = serde_json::to_value(export).unwrap();
        for key in ["label", "side_rule", "type_packages"] {
            assert!(carried.get(key).is_none(), "{key} is written though absent");
        }
    }

    #[test]
    fn an_answer_that_is_not_the_contract_s_shape_is_no_claims() {
        assert_eq!(parse_answer("not json"), LibraryAnswer::default());
        assert_eq!(
            parse_answer(r#"{"success":true,"text":"{}"}"#),
            LibraryAnswer::default()
        );
    }

    #[test]
    fn claims_for_a_version_the_service_did_not_ask_about_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        install_package(dir.path(), "lib", "2.0.0");
        npm_lockfile(dir.path(), &[("lib", PUBLIC_TGZ)]);
        let asked = request(&specifiers(&["lib"]), &install_at(dir.path())).unwrap();
        assert!(
            asked
                .claims(parse_answer(STORE_ANSWERED).exports)
                .is_empty()
        );
    }

    fn asked_lib_and_other(dir: &Path) -> Asked {
        install_package(dir, "lib", "1.0.0");
        install_package(dir, "other", "1.0.0");
        npm_lockfile(dir, &[("lib", PUBLIC_TGZ), ("other", PUBLIC_TGZ)]);
        request(&specifiers(&["lib", "other"]), &install_at(dir)).unwrap()
    }

    /// carrick#1066. The turn is taken before the ask and held until it has
    /// its answer, so a second service asking the same thing sends after it.
    #[tokio::test]
    async fn an_ask_holds_its_turn_from_before_it_is_sent_until_it_is_answered() {
        let turn_held = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct Turn(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Turn {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let held = turn_held.clone();
        let during = turn_held.clone();
        let answer = ask(
            LibraryClaimsRequest {
                roles: ROLES.to_vec(),
                packages: Vec::new(),
            },
            move |_| async move {
                held.store(true, std::sync::atomic::Ordering::SeqCst);
                Turn(held)
            },
            move |_| async move {
                assert!(
                    during.load(std::sync::atomic::Ordering::SeqCst),
                    "the turn is held while the ask is in flight"
                );
                Ok(String::new())
            },
        )
        .await;
        assert_eq!(answer, LibraryAnswer::default());
        assert!(
            !turn_held.load(std::sync::atomic::Ordering::SeqCst),
            "the turn is released once the ask is answered"
        );
    }

    #[tokio::test]
    async fn a_refused_ask_is_no_claims_and_never_an_error() {
        let refused = ask(
            LibraryClaimsRequest {
                roles: ROLES.to_vec(),
                packages: Vec::new(),
            },
            |_| async {},
            |_| async {
                Err(AgentCallError::permanent(
                    "scan_not_started",
                    "no scan is running for this credential".to_string(),
                ))
            },
        )
        .await;
        assert_eq!(refused, LibraryAnswer::default());
    }

    /// The store's pending body for `lib` with `reason`, as `http.ts` sends it.
    fn pending_for(reason: &str) -> String {
        format!(
            r#"{{"library_claims":[{{"package":"lib","version":"1.0.0","status":"pending","reason":"{reason}"}}]}}"#
        )
    }

    /// A fake store: the nth ask gets `bodies[n]` (the last one repeats; an
    /// empty string is a refusal), and every request is kept with the
    /// (paused) clock's reading when it arrived.
    struct FakeStore {
        bodies: Vec<String>,
        seen: std::sync::Mutex<Vec<(Duration, Vec<String>)>>,
        start: tokio::time::Instant,
    }

    impl FakeStore {
        fn new(bodies: Vec<String>) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                bodies,
                seen: Default::default(),
                start: tokio::time::Instant::now(),
            })
        }

        async fn send(&self, request: LibraryClaimsRequest) -> Result<String, AgentCallError> {
            let mut seen = self.seen.lock().unwrap();
            let n = seen.len();
            seen.push((
                self.start.elapsed(),
                request.packages.iter().map(|p| p.package.clone()).collect(),
            ));
            match &self.bodies[n.min(self.bodies.len() - 1)] {
                body if body.is_empty() => Err(AgentCallError::permanent(
                    "scan_not_started",
                    "no scan is running for this credential".to_string(),
                )),
                body => Ok(body.clone()),
            }
        }

        async fn run(self: &std::sync::Arc<Self>, asked: &Asked) -> Settled {
            let store = self.clone();
            ask_until_settled(
                asked,
                |_| async {},
                move |request| {
                    let store = store.clone();
                    async move { store.send(request).await }
                },
                None,
            )
            .await
        }

        fn asks(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_package_being_built_is_waited_for_and_read_once_answered() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        let store = FakeStore::new(vec![
            pending_for("in_flight"),
            pending_for("in_flight"),
            pending_for("building"),
            STORE_ANSWERED.to_string(),
        ]);
        let settled = store.run(&asked).await;
        assert_eq!(settled.exports.len(), 1, "the claims are read");
        assert!(settled.pending.is_empty());
        assert_eq!(store.asks(), 4);
        let seen = store.seen.lock().unwrap();
        assert_eq!(
            seen[1].0, POLL_INTERVAL,
            "the first poll is one interval on"
        );
        assert_eq!(seen[3].0, POLL_INTERVAL * 3);
        assert_eq!(seen[1].1, vec!["lib".to_string()], "asked alone");
    }

    #[tokio::test(start_paused = true)]
    async fn a_package_still_building_at_the_cap_stops_the_wait_and_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        let store = FakeStore::new(vec![pending_for("in_flight")]);
        let settled = store.run(&asked).await;
        assert_eq!(settled.pending, specifiers(&["lib"]));
        assert!(settled.exports.is_empty());
        let seen = store.seen.lock().unwrap();
        // Ruled 2026-10-17: every 10 s, for 10 minutes from the first ask.
        assert_eq!(POLL_INTERVAL, Duration::from_secs(10));
        assert_eq!(WAIT_CAP, Duration::from_secs(600));
        assert_eq!(seen.len(), 60, "the first ask at 0, then one per interval");
        assert_eq!(seen.last().unwrap().0, Duration::from_secs(590));
    }

    #[tokio::test(start_paused = true)]
    async fn a_reason_no_build_can_end_is_not_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        for reason in [
            "lister_unavailable",
            "classifier_unavailable",
            "picker_unavailable",
            "store_unavailable",
            "store_row_unreadable",
            "dispatch_failed",
        ] {
            let store = FakeStore::new(vec![pending_for(reason)]);
            let settled = store.run(&asked).await;
            assert_eq!(store.asks(), 1, "{reason}: no second ask");
            assert_eq!(settled.pending, specifiers(&["lib"]), "{reason}: named");
        }
        let failed = r#"{"library_claims":[{"package":"lib","version":"1.0.0","status":"skipped","reason":"build_failed"}]}"#;
        let store = FakeStore::new(vec![failed.to_string()]);
        let settled = store.run(&asked).await;
        assert_eq!(
            store.asks(),
            1,
            "a failed build due a retry later is not waited for"
        );
        assert!(settled.pending.is_empty(), "and it is skipped, not pending");
        assert_eq!(settled.build_failed, specifiers(&["lib"]), "but named");
        let invalid = r#"{"library_claims":[{"package":"lib","version":"1.0.0","status":"skipped","reason":"invalid_package"}]}"#;
        let settled = FakeStore::new(vec![invalid.to_string()]).run(&asked).await;
        assert!(settled.build_failed.is_empty(), "no other skipped reason");
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_package_being_built_is_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        let body = r#"{"library_claims":[
            {"package":"lib","version":"1.0.0","status":"pending","reason":"in_flight"},
            {"package":"other","version":"1.0.0","status":"pending","reason":"lister_unavailable"}]}"#;
        let store = FakeStore::new(vec![body.to_string(), STORE_ANSWERED.to_string()]);
        let settled = store.run(&asked).await;
        assert_eq!(store.asks(), 2);
        assert_eq!(store.seen.lock().unwrap()[1].1, vec!["lib".to_string()]);
        assert_eq!(settled.pending, specifiers(&["other"]));
        assert_eq!(settled.exports.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn every_package_answered_first_time_asks_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        let store = FakeStore::new(vec![STORE_ANSWERED.to_string()]);
        let settled = store.run(&asked).await;
        assert_eq!(store.asks(), 1);
        assert_eq!(store.start.elapsed(), Duration::ZERO, "and waits nothing");
        assert_eq!(settled.exports.len(), 1);
        assert!(settled.pending.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_store_that_stops_answering_ends_the_wait_and_never_fails_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        let store = FakeStore::new(vec![pending_for("building"), String::new()]);
        let settled = store.run(&asked).await;
        assert_eq!(store.asks(), 2, "one refused poll ends it");
        assert_eq!(settled.pending, specifiers(&["lib"]));
        let store = FakeStore::new(vec![String::new()]);
        let settled = store.run(&asked).await;
        assert_eq!(store.asks(), 1);
        assert!(settled.exports.is_empty());
        assert!(
            settled.pending.is_empty(),
            "a store that never answered named nothing pending"
        );
    }

    /// carrick#2613: the first answer goes out the moment it arrives, while
    /// the wait for the package still building carries on, and it names what
    /// it leaves without exports.
    #[tokio::test(start_paused = true)]
    async fn the_first_answer_is_sent_before_the_wait_for_a_building_package_ends() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        let store = FakeStore::new(vec![pending_for("in_flight"), STORE_ANSWERED.to_string()]);
        let (early, first) = tokio::sync::oneshot::channel();
        let sent = store.clone();
        let (settled, (answer, got_it_at)) = tokio::join!(
            ask_until_settled(
                &asked,
                |_| async {},
                move |request| {
                    let store = sent.clone();
                    async move { store.send(request).await }
                },
                Some(early),
            ),
            async {
                let answer = first.await.expect("the first answer is always sent");
                (answer, store.start.elapsed())
            },
        );
        assert_eq!(got_it_at, Duration::ZERO, "sent when it arrived");
        assert!(store.start.elapsed() >= POLL_INTERVAL, "the wait went on");
        assert!(
            answer.exports.is_empty(),
            "the first answer holds no claims"
        );
        assert_eq!(answer.unanswered, specifiers(&["lib"]));
        assert!(!answer.ask_failed);
        assert_eq!(settled.exports.len(), 1, "the settled answer has them");
    }

    /// carrick#2613: an ask that got no answer still sends, with every
    /// package asked left unanswered, so the fact layer fails closed.
    #[tokio::test(start_paused = true)]
    async fn a_failed_ask_sends_every_package_asked_as_unanswered() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        let store = FakeStore::new(vec![String::new()]);
        let (early, mut first) = tokio::sync::oneshot::channel();
        let sent = store.clone();
        ask_until_settled(
            &asked,
            |_| async {},
            move |request| {
                let store = sent.clone();
                async move { store.send(request).await }
            },
            Some(early),
        )
        .await;
        let answer = first.try_recv().expect("sent on the failed path too");
        assert!(answer.ask_failed);
        assert!(answer.exports.is_empty());
        assert_eq!(answer.unanswered, specifiers(&["lib", "other"]));
    }

    /// carrick#2725: the first answer names as answered only the packages
    /// this service asked about, at the version it asked, as its exports
    /// are kept. A package the answer holds for anything else (another
    /// service's ask, another version) is not answered here: its exports
    /// are dropped, so a call into it would read as answered with nothing
    /// to label, and the fact layer would call that walk finished.
    #[tokio::test(start_paused = true)]
    async fn the_first_answer_names_answered_only_what_this_service_asked() {
        let dir = tempfile::tempdir().unwrap();
        let asked = asked_lib_and_other(dir.path());
        let body = r#"{"library_claims":[
            {"package":"lib","version":"1.0.0","status":"answered","exports":[]},
            {"package":"other","version":"2.0.0","status":"answered","exports":[]},
            {"package":"framework","version":"1.0.0","status":"answered","exports":[
                {"specifier":"framework","export":"default","role":"server_framework",
                 "side":null,"makes":[],"scopes":[],"ops":[],"reserved":[],"calls":[],"patterns":[]}]}]}"#;
        let store = FakeStore::new(vec![body.to_string()]);
        let (early, mut first) = tokio::sync::oneshot::channel();
        let sent = store.clone();
        ask_until_settled(
            &asked,
            |_| async {},
            move |request| {
                let store = sent.clone();
                async move { store.send(request).await }
            },
            Some(early),
        )
        .await;
        let answer = first.try_recv().expect("the first answer is sent");
        assert!(answer.exports.is_empty(), "no export of an unasked package");
        assert_eq!(answer.answered, specifiers(&["lib"]));
    }

    #[test]
    fn the_store_s_pending_reasons_split_into_waited_and_not() {
        let answer = parse_answer(
            r#"{"library_claims":[
                {"package":"a","version":"1.0.0","status":"pending","reason":"in_flight"},
                {"package":"b","version":"1.0.0","status":"pending","reason":"building"},
                {"package":"c","version":"1.0.0","status":"pending","reason":"lister_unavailable"},
                {"package":"d","version":"1.0.0","status":"pending"}]}"#,
        );
        assert_eq!(answer.pending, specifiers(&["a", "b", "c", "d"]));
        assert_eq!(answer.building, specifiers(&["a", "b"]));
    }
}
