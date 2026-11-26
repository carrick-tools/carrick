//! Producer rows stated by a type (tables, views and functions of a hosted
//! database): the keys of a resource type, read by the compiler engine at
//! the type argument of a served package's make call, each one row.
//!
//! The convention is plain data ([`ResourceTypeConvention`], beside the
//! routing conventions in [`crate::file_based_router`]); the keys are the
//! compiler engine's `resource_keys` read, which holds no library name and
//! no path template. The make call is found through the settled make claim
//! of the served package, as [`served_makers`] finds the maker calls the
//! learned bases read. This module joins the three and says which keys are
//! this service's rows:
//!
//! - a key declared outside the service, in a package under `node_modules`,
//!   or with no declaration states no row (a type another service declares
//!   is that service's row, and this scan does not see its make);
//! - two makes of one type state each key once.
//!
//! The rows carry no type entries (the judge would compare a key's members
//! with a typed client, which is a column-level verdict this stage does not
//! give): [`crate::engine`] keeps them out of the type manifest.

use crate::file_based_router::ResourceTypeConvention;
use crate::library_claims::ExportClaims;
use crate::position_rows::Kind;
use crate::request_summary::decided_sites::{make_claim, served_makers};
use crate::request_summary::{LibrarySites, RequestSummaryInputs};
use crate::services::type_sidecar::{ResourceKeysAnswer, ResourceKeysQuery};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};

/// What a key of a resource type is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Table,
    View,
    Function,
}

/// The resource a row is, as a type states it: the key a realtime listener
/// and a REST row both derive is `<schema>.<name>`. Optional on the index
/// blob and outside the row's key and every hash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Resource {
    pub schema: String,
    pub name: String,
    pub kind: ResourceKind,
}

/// One row a resource type states.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TypeResourceRow {
    pub resource: Resource,
    pub method: String,
    pub path: String,
    /// The key's declaring file, relative to the scan root.
    pub file: String,
    /// The line the key is declared on.
    pub line: u32,
}

/// The rows the resource types of `conventions` state in this service. `ask`
/// gets one question per make call of a served package, in order, and
/// answers one per question; an `Err` or a short answer states no row.
/// No marker (no convention), no make or no rows asks nothing.
pub fn rows_of<E: std::fmt::Display>(
    inputs: &RequestSummaryInputs,
    library: &LibrarySites,
    exports: &[ExportClaims],
    conventions: &[ResourceTypeConvention],
    ask: impl FnOnce(&[ResourceKeysQuery]) -> Result<Vec<ResourceKeysAnswer>, E>,
) -> Vec<TypeResourceRow> {
    let Some(positions) = inputs.positions.as_ref() else {
        return Vec::new();
    };
    // Each question with the convention (by index) whose groups it asks.
    let mut questions: Vec<(usize, ResourceKeysQuery)> = Vec::new();
    for (at, convention) in conventions.iter().enumerate() {
        let served = BTreeSet::from([convention.served_package.clone()]);
        for (site, export) in served_makers(library, exports, &served) {
            if site.member.is_some() || make_claim(export, site.form, None).is_none() {
                continue;
            }
            let Ok(row) = positions.lookup(&site.file, site.span_start, site.span_end, Kind::Call)
            else {
                continue;
            };
            questions.push((
                at,
                ResourceKeysQuery {
                    file: site.file.to_string_lossy().into_owned(),
                    start: row.start(),
                    end: row.end(),
                    type_arg: convention.type_arg,
                    paths: convention.groups.iter().map(|g| g.at.clone()).collect(),
                },
            ));
        }
    }
    if questions.is_empty() {
        return Vec::new();
    }
    let asked: Vec<ResourceKeysQuery> = questions.iter().map(|(_, q)| q.clone()).collect();
    let answers = match ask(&asked) {
        Ok(answers) if answers.len() == asked.len() => answers,
        Ok(answers) => {
            tracing::warn!(
                asked = asked.len(),
                answered = answers.len(),
                "resource keys: the answer does not match the question; no row stated"
            );
            return Vec::new();
        }
        Err(error) => {
            tracing::warn!(%error, "resource keys: the ask failed; no row stated");
            return Vec::new();
        }
    };
    let mut rows: BTreeSet<TypeResourceRow> = BTreeSet::new();
    for ((at, _), answer) in questions.iter().zip(&answers) {
        let convention = &conventions[*at];
        for read in &answer.paths {
            let Some(group) = convention.groups.iter().find(|g| g.at == read.path) else {
                continue;
            };
            // The schema is the first step of the path the answer read.
            let Some(schema) = read.path.first() else {
                continue;
            };
            for key in &read.keys {
                let (Some(file), Some(line)) = (key.decl_file.as_deref(), key.decl_line) else {
                    continue;
                };
                if !is_in_service(file) {
                    continue;
                }
                rows.insert(TypeResourceRow {
                    resource: Resource {
                        schema: schema.clone(),
                        name: key.name.clone(),
                        kind: group.kind,
                    },
                    method: group.method.clone(),
                    path: group.path.replace("{key}", &key.name),
                    file: file.to_string(),
                    line,
                });
            }
        }
    }
    rows.into_iter().collect()
}

/// Whether `file` (relative to the scan root) is one of the service's own:
/// below the root and in no package folder.
fn is_in_service(file: &str) -> bool {
    let path = Path::new(file);
    !path.is_absolute()
        && path.components().all(|part| match part {
            Component::Normal(name) => name != "node_modules",
            Component::CurDir => true,
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_outside_the_service_or_in_a_package_is_not_its_row() {
        assert!(is_in_service("src/types.ts"));
        assert!(is_in_service("./src/types.ts"));
        assert!(!is_in_service("../shared/types.ts"));
        assert!(!is_in_service("node_modules/@scope/pkg/index.d.ts"));
        assert!(!is_in_service("/abs/types.ts"));
    }
}
