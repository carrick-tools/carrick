//! Where a manifest entry's type is declared (carrick#2148, H2).
//!
//! The compiler engine reports, with every answer that names a type, the
//! declaration of the type it printed (`TypeHomeReport`: the symbol, the
//! declaring file past any barrel, and the line of the declaration's name).
//! [`store_type_homes`] is the one writer of a manifest entry's
//! `defined_in`: it runs once, after every entry exists and every answer is
//! in, and states the home of the answer whose text the entry published.
//! The anchor passes in `engine/mod.rs` write symbols only.
//!
//! The rule, in one place:
//! - The answer that published the text is the bundle answer for an explicit
//!   entry, otherwise the first inference by alias: the precedence
//!   `enrich_manifest_with_type_resolution` reads the text by.
//! - An entry with a symbol takes a home only when that home names the same
//!   symbol (SDL marks `[ ]` and `!` stripped). Where a pass anchored the
//!   entry from the source's stated root or from a re-aimed root, that root's
//!   home counts too. A home for another type is never stored.
//! - An entry with no symbol takes the published answer's home as it is; only
//!   `defined_in` carries the name. A home marked `kind: schema` (H3) is the
//!   `const` of the schema a request was validated by, and only ever reaches
//!   an entry this way: the engine names no symbol for a schema contract.
//! - The file is stored repo-relative. A file outside the repo is no home.

use crate::cloud_storage::{ManifestRole, TypeHome, TypeManifestEntry};
use crate::operation::OperationKey;
use crate::services::type_sidecar::{TypeHomeReport, TypeResolutionResult};
use std::collections::HashMap;
use std::path::Path;

/// A symbol as a home names it: GraphQL list and non-null marks are not part
/// of the declared name.
fn bare_symbol(symbol: &str) -> &str {
    symbol.trim_matches(|c| matches!(c, '[' | ']' | '!'))
}

/// `file` as a path relative to the repo, when it lies inside it. The engine
/// reports a resolved absolute path that may spell the repo root differently
/// from the scan (`/private/var/...` on macOS), so both forms are tried. A
/// package installed under the repo keeps its installed path (carrick#1819).
pub(crate) fn inside_repo(file: &str, repo_path: &str) -> Option<String> {
    let declaring = Path::new(file);
    let inside = |base: &Path| {
        declaring
            .strip_prefix(base)
            .ok()
            .filter(|rest| rest.is_relative())
            .map(Path::to_path_buf)
    };
    let repo = Path::new(repo_path);
    let relative = inside(repo).or_else(|| inside(&repo.canonicalize().ok()?))?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// Write `defined_in` on every entry from the answers in `resolution`.
pub fn store_type_homes(
    manifest: &mut [TypeManifestEntry],
    resolution: &TypeResolutionResult,
    repo_path: &str,
) {
    // The bundle answers, as enrichment reads them (a later one replaces).
    let mut explicit: HashMap<&str, Option<&TypeHomeReport>> = HashMap::new();
    for entry in &resolution.explicit_manifest {
        explicit.insert(entry.alias.as_str(), entry.home.as_ref());
    }
    // The first inference per alias.
    let mut first = HashMap::new();
    for inferred in &resolution.inferred_types {
        first.entry(inferred.alias.as_str()).or_insert(inferred);
    }
    // The root each re-aimed op was anchored at, by the op's site. The change
    // names one alias; every entry of the op at that site took the root.
    let reaimed: Vec<(OperationKey, ManifestRole, String, u32, TypeHomeReport)> = resolution
        .anchor_changes
        .iter()
        .filter_map(|change| {
            let site = manifest
                .iter()
                .find(|entry| entry.type_alias == change.alias)?;
            let home = change.reaimed.as_ref()?.home.as_ref()?;
            Some((
                site.key.clone(),
                site.role,
                site.file_path.clone(),
                site.line_number,
                home.clone(),
            ))
        })
        .collect();

    for entry in manifest.iter_mut() {
        let alias = entry.type_alias.as_str();
        let published = match explicit.get(alias) {
            Some(home) => *home,
            None => first
                .get(alias)
                .and_then(|inferred| inferred.type_home.as_ref()),
        };
        let chosen: Option<&TypeHomeReport> = match entry.primary_type_symbol.as_deref() {
            None => published,
            Some(symbol) => {
                let symbol = bare_symbol(symbol);
                let stated = first
                    .get(alias)
                    .and_then(|inferred| inferred.stated_body.as_ref())
                    .and_then(|stated| stated.root_home.as_ref());
                let from_reaim = reaimed
                    .iter()
                    .filter(|(key, role, file, line, _)| {
                        *key == entry.key
                            && *role == entry.role
                            && *file == entry.file_path
                            && *line == entry.line_number
                    })
                    .map(|(.., home)| home);
                published
                    .into_iter()
                    .chain(stated)
                    .chain(from_reaim)
                    .find(|home| bare_symbol(&home.symbol) == symbol)
            }
        };
        entry.defined_in = chosen.and_then(|home| {
            Some(TypeHome {
                kind: home.kind,
                file_path: inside_repo(&home.file_path, repo_path)?,
                line_number: home.line,
                symbol: home.symbol.clone(),
            })
        });
    }
}
