//! The full path a route site is served at, per instance end (carrick#2474
//! R2b). Record-only, with the receiver trace on.
//!
//! A site's instance ends ([`super::instances`]) say what its labelled
//! call is made on: a maker's result, a scope a labelled member opens, or a
//! hole. A maker's result is served wherever a member labelled with
//! `mounts` is handed it ([`mount_edges`], read once per service), under
//! that member's prefix, below that member's own receiver; one handed to
//! no mount is served at the top. The composed path joins every prefix
//! from the top down with the path the route's own call writes. Every
//! stated value is recorded, one path each (#2474 Q2). A walk that ends at
//! a hole still composes the prefixes below it, and says the path is
//! relative to that hole (`end`); a prefix that is not text composes no
//! path.

use std::path::Path;

use serde::Serialize;

use super::super::RequestSummaryInputs;
use super::boundaries::{Above, Boundary, End, Part};
use super::instances::{
    InstanceAt, InstanceEnd, InstanceWalk, MakerLabel, Prefix, ScopeBinds, ScopeLabel, WalkIndex,
};
use super::{Calls, Placed, relative};

/// One path a route site is served at.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Composed {
    /// The path: every prefix from where the walk up ends down, joined
    /// with the route's own; `None` where a prefix or the route's own path
    /// is not text. Full only where `end` is `top`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Each scope or mount edge, from the top down: `file:line member
    /// prefix`.
    pub edges: Vec<String>,
    /// Where the walk up ends: `top` (an instance served at no mount), a
    /// hole's reason (the path is relative to what serves the instance
    /// there), or `cycle`.
    pub end: String,
}

/// An instance served under a prefix: a call of a member labelled with
/// `mounts`, handed a labelled maker's result.
#[derive(Debug, Clone)]
pub(super) struct MountEdge {
    /// The maker call of the instance handed in (`file`, `line`).
    child: (String, u32),
    /// The mount call, `file:line`.
    at: String,
    member: String,
    prefix: Prefix,
    /// The ends of the mount call's own receiver the label applies to.
    parents: Vec<InstanceEnd>,
    /// Made in a file only tests import (carrick#2775); kept only while no
    /// mount made elsewhere hands the same instance.
    in_test_only: bool,
}

/// What a service's instances are mounted under (carrick#2474 R3 S0).
#[derive(Debug, Default)]
pub(super) struct Mounts {
    /// Every verified mount edge.
    edges: Vec<MountEdge>,
    /// The maker calls (`file`, `line`) whose result is handed to a member
    /// call of an instance of the same package that no verified edge
    /// reads: a `mounts` claim that did not verify, a member no label
    /// names, or an argument the label does not name. Such an instance is
    /// served somewhere the trace cannot say, so it is not "the top".
    unverified: Vec<(String, u32)>,
    /// The mount calls (`file:line`) the trace ignores because a file only
    /// tests import makes them (carrick#2775): a helper that builds its own
    /// app for the suite serves nothing in production. Each would have been
    /// a verified edge or an unverified root; neither is recorded.
    pub(super) test_only: Vec<String>,
}

/// The service's mounts: the verified edges, and the instances handed to a
/// member call no verified edge reads.
pub(super) fn mount_edges<'a>(
    calls: &Calls<'a>,
    inputs: &RequestSummaryInputs,
    root: &Path,
    makers: &[MakerLabel],
    scopes: &[ScopeLabel],
    index: &WalkIndex<'a>,
) -> Mounts {
    let (edges, mut test_only) = verified_edges(calls, inputs, root, makers, scopes, index);
    let (unverified, ignored) =
        unverified_roots(calls, inputs, root, makers, scopes, index, &edges);
    test_only.extend(ignored);
    test_only.sort();
    test_only.dedup();
    Mounts {
        edges,
        unverified,
        test_only,
    }
}

/// Every instance handed, at any argument, to a library call made on an
/// instance of the same package, that no verified edge in `edges` reads
/// (that call and that instance), and the mount calls (`file:line`) a test-only
/// file makes, which are neither (carrick#2775).
fn unverified_roots<'a>(
    calls: &Calls<'a>,
    inputs: &RequestSummaryInputs,
    root: &Path,
    makers: &[MakerLabel],
    scopes: &[ScopeLabel],
    index: &WalkIndex<'a>,
    edges: &[MountEdge],
) -> (Vec<(String, u32)>, Vec<String>) {
    let mut placed: Vec<Placed<'a>> = calls
        .by_span
        .values()
        .filter(|placed| !placed.call.args.is_empty() || !placed.call.instance.args.is_empty())
        .copied()
        .collect();
    placed.sort_by(|a, b| {
        (a.file, a.call.site.span_start, a.call.site.span_end).cmp(&(
            b.file,
            b.call.site.span_start,
            b.call.site.span_end,
        ))
    });
    let walk = || InstanceWalk::new(calls, inputs, root, makers, scopes, index);
    let mut out: Vec<(String, u32)> = Vec::new();
    let mut test_only: Vec<String> = Vec::new();
    let mut held: Vec<(String, (String, u32))> = Vec::new();
    for placed in placed {
        if !walk().library_call(placed.file, placed.call) {
            continue;
        }
        let receivers = walk().of(&[placed]);
        let packages: Vec<&str> = receivers
            .iter()
            .filter_map(|end| match &end.end {
                InstanceAt::Root { package, .. } | InstanceAt::Scope { package, .. } => {
                    Some(package.as_str())
                }
                InstanceAt::Hole { .. } | InstanceAt::Handed { .. } => None,
            })
            .collect();
        if packages.is_empty() {
            continue;
        }
        let at = format!("{}:{}", relative(root, placed.file), placed.call.site.line);
        for arg in 0..placed.call.args.len().max(placed.call.instance.args.len()) {
            for child in walk().of_arg(placed, arg, None) {
                let InstanceAt::Root {
                    file,
                    line,
                    package,
                    ..
                } = &child.end
                else {
                    continue;
                };
                let here = (file.clone(), *line);
                let read = edges.iter().any(|edge| edge.child == here && edge.at == at);
                if !packages.contains(&package.as_str()) || read {
                    continue;
                }
                if inputs.test_only.contains(placed.file) {
                    held.push((at.clone(), here));
                } else if !out.contains(&here) {
                    out.push(here);
                }
            }
        }
    }
    // A test-only file's mount is ignored only when the same instance is
    // also handed from a file that is not test-only; otherwise (an entry only
    // a test imports) it is the instance's only mount and stays.
    for (at, here) in held {
        let elsewhere = out.contains(&here)
            || edges
                .iter()
                .any(|edge| edge.child == here && !edge.in_test_only);
        if elsewhere {
            test_only.push(at);
        } else if !out.contains(&here) {
            out.push(here);
        }
    }
    (out, test_only)
}

/// Every verified mount edge of the service: each call of a member a
/// `mounts` label names, made on an instance the label applies to, with
/// each maker's result it is handed; and the mount calls (`file:line`) made in
/// a test-only file, which are not edges (carrick#2775).
fn verified_edges<'a>(
    calls: &Calls<'a>,
    inputs: &RequestSummaryInputs,
    root: &Path,
    makers: &[MakerLabel],
    scopes: &[ScopeLabel],
    index: &WalkIndex<'a>,
) -> (Vec<MountEdge>, Vec<String>) {
    let mounts: Vec<&ScopeLabel> = scopes
        .iter()
        .filter(|label| matches!(label.binds, ScopeBinds::Mounts { .. }))
        .collect();
    if mounts.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut placed: Vec<Placed<'a>> = calls
        .by_span
        .values()
        .filter(|placed| {
            mounts
                .iter()
                .any(|label| label.member == placed.call.callee)
        })
        .copied()
        .collect();
    placed.sort_by(|a, b| {
        (a.file, a.call.site.span_start, a.call.site.span_end).cmp(&(
            b.file,
            b.call.site.span_start,
            b.call.site.span_end,
        ))
    });
    let walk = || InstanceWalk::new(calls, inputs, root, makers, scopes, index);
    let mut edges = Vec::new();
    for placed in placed {
        if !walk().library_call(placed.file, placed.call) {
            continue;
        }
        let receivers = walk().of(&[placed]);
        for label in mounts
            .iter()
            .filter(|label| label.member == placed.call.callee)
        {
            let ScopeBinds::Mounts { arg, key } = &label.binds else {
                continue;
            };
            let parents: Vec<InstanceEnd> = receivers
                .iter()
                .filter(|end| label.applies_to(end))
                .cloned()
                .collect();
            if parents.is_empty() {
                continue;
            }
            for child in walk().of_arg(placed, *arg, key.as_deref()) {
                let InstanceAt::Root { file, line, .. } = &child.end else {
                    continue;
                };
                let at = format!("{}:{}", relative(root, placed.file), placed.call.site.line);
                edges.push(MountEdge {
                    child: (file.clone(), *line),
                    at,
                    member: label.member.clone(),
                    prefix: walk().prefix_of(&placed, &label.name, None),
                    parents: parents.clone(),
                    in_test_only: inputs.test_only.contains(placed.file),
                });
            }
        }
    }
    // A mount a test-only file makes is dropped when a file that is not
    // test-only also hands the same instance; an entry only a test imports
    // is its instance's only mount and stays.
    let served: Vec<(String, u32)> = edges
        .iter()
        .filter(|edge| !edge.in_test_only)
        .map(|edge| edge.child.clone())
        .collect();
    let mut test_only: Vec<String> = Vec::new();
    edges.retain(|edge| {
        let drop = edge.in_test_only && served.contains(&edge.child);
        if drop {
            test_only.push(edge.at.clone());
        }
        !drop
    });
    (edges, test_only)
}

/// One walk from an instance end up to the top: the prefixes and edges
/// from the top down, and where it ends.
#[derive(Clone)]
struct Chain {
    prefixes: Vec<Prefix>,
    edges: Vec<String>,
    end: String,
    /// Where the chain leaves the service, for a hole that names it
    /// (carrick#2474 R5a).
    boundary: Option<Boundary>,
}

fn edge_text(at: &str, member: &str, prefix: &Prefix) -> String {
    let prefix = match prefix {
        Prefix::Unstated => "-".to_string(),
        Prefix::Stated { values } => values.join("|"),
        Prefix::Unread { text } => format!("?{}", text.as_deref().unwrap_or_default()),
    };
    format!("{at} {member} {prefix}")
}

/// The most chains and paths one site composes: each mount edge and each
/// scope's parent multiplies the chains, and each stated value the paths,
/// with no bound of their own.
pub(super) const MAX_COMPOSED: usize = 64;

/// What is left of one site's [`MAX_COMPOSED`], and whether it trimmed.
struct Budget {
    left: usize,
    trimmed: bool,
}

impl Budget {
    /// Takes one, or says the cap is reached.
    fn take(&mut self) -> bool {
        if self.left == 0 {
            self.trimmed = true;
            return false;
        }
        self.left -= 1;
        true
    }
}

/// One chain per walk up from `end` to where it ends, each walk's end
/// taking one of `budget`.
fn chains(
    end: &InstanceEnd,
    mounts: &Mounts,
    seen: &mut Vec<(String, u32)>,
    budget: &mut Budget,
) -> Vec<Chain> {
    if budget.left == 0 {
        budget.trimmed = true;
        return Vec::new();
    }
    let ended = |end: String, boundary: Option<Boundary>, budget: &mut Budget| {
        if budget.take() {
            vec![Chain {
                prefixes: Vec::new(),
                edges: Vec::new(),
                end,
                boundary,
            }]
        } else {
            Vec::new()
        }
    };
    match &end.end {
        // A socket library's call hands the instance in: nothing mounts it
        // under a prefix (carrick#1670).
        InstanceAt::Handed { .. } => ended("handed".to_string(), None, budget),
        InstanceAt::Hole {
            reason, boundary, ..
        } => ended(
            serde_json::to_value(reason)
                .ok()
                .and_then(|reason| reason.as_str().map(str::to_string))
                .unwrap_or_default(),
            boundary.clone(),
            budget,
        ),
        InstanceAt::Root { file, line, .. } => {
            let here = (file.clone(), *line);
            if seen.contains(&here) {
                return ended("cycle".to_string(), None, budget);
            }
            let served: Vec<&MountEdge> = mounts
                .edges
                .iter()
                .filter(|edge| edge.child == here)
                .collect();
            // Handed to a call no verified edge reads: served somewhere the
            // trace cannot say, so its paths are relative (carrick#2474
            // R3 S0).
            let unverified = mounts.unverified.contains(&here);
            if served.is_empty() {
                let end = if unverified {
                    "mount_unverified"
                } else {
                    "top"
                };
                return ended(end.to_string(), None, budget);
            }
            seen.push(here.clone());
            let mut out = if unverified {
                ended("mount_unverified".to_string(), None, budget)
            } else {
                Vec::new()
            };
            for edge in served {
                for parent in &edge.parents {
                    for mut chain in chains(parent, mounts, seen, budget) {
                        chain.prefixes.push(edge.prefix.clone());
                        chain
                            .edges
                            .push(edge_text(&edge.at, &edge.member, &edge.prefix));
                        out.push(chain);
                    }
                }
            }
            seen.pop();
            out
        }
        InstanceAt::Scope {
            file,
            line,
            member,
            prefix,
            parents,
            ..
        } => {
            let at = format!("{file}:{line}");
            let mut out = Vec::new();
            for parent in parents {
                for mut chain in chains(parent, mounts, seen, budget) {
                    chain.prefixes.push(prefix.clone());
                    chain.edges.push(edge_text(&at, member, prefix));
                    out.push(chain);
                }
            }
            out
        }
    }
}

/// `parts` joined as one path, each prefix before the next
/// ([`crate::utils::join_prefix_and_path`]).
fn join_path(parts: &[&str]) -> String {
    parts.iter().fold(String::new(), |path, part| {
        crate::utils::join_prefix_and_path(&path, part)
    })
}

/// Each chain up `ends`, at most [`MAX_COMPOSED`], as a boundary record
/// holds it (carrick#2474 R5a): its edges and prefixes from where it ends
/// down, and that end.
pub(super) fn aboves(ends: &[InstanceEnd], mounts: &Mounts) -> Vec<Above> {
    let mut budget = Budget {
        left: MAX_COMPOSED,
        trimmed: false,
    };
    let mut out: Vec<Above> = Vec::new();
    for end in ends {
        for chain in chains(end, mounts, &mut Vec::new(), &mut budget) {
            let parts = chain
                .edges
                .into_iter()
                .zip(chain.prefixes)
                .flat_map(|(edge, prefix)| [Part::Edge { edge }, Part::Prefix { prefix }])
                .collect();
            let end = match (chain.end.as_str(), chain.boundary) {
                ("top", _) => End::Top,
                ("mount_unverified", _) => End::MountUnverified,
                (_, Some(boundary)) => End::Boundary { boundary },
                (reason, None) => End::Hole {
                    reason: reason.to_string(),
                },
            };
            let above = Above { parts, end };
            if !out.contains(&above) {
                out.push(above);
            }
        }
    }
    out
}

/// The paths a route site whose call writes `route`, made on `ends`, is
/// served at: one per chain up each end, and per stated prefix value, at
/// most [`MAX_COMPOSED`] of each; true where the cap left some out.
pub(super) fn compose(
    ends: &[InstanceEnd],
    route: Option<&str>,
    mounts: &Mounts,
) -> (Vec<Composed>, bool) {
    let mut out: Vec<Composed> = Vec::new();
    let mut budget = Budget {
        left: MAX_COMPOSED,
        trimmed: false,
    };
    let mut all: Vec<Chain> = Vec::new();
    for end in ends {
        all.extend(chains(end, mounts, &mut Vec::new(), &mut budget));
    }
    for chain in all {
        // Every stated value, one path each.
        let mut paths: Vec<Option<Vec<String>>> = vec![Some(Vec::new())];
        let mut unread = false;
        for prefix in &chain.prefixes {
            match prefix {
                Prefix::Unstated => {}
                Prefix::Stated { values } => {
                    paths = paths
                        .into_iter()
                        .flat_map(|path| {
                            values.iter().map(move |value| {
                                path.clone().map(|mut parts| {
                                    parts.push(value.clone());
                                    parts
                                })
                            })
                        })
                        .take(MAX_COMPOSED + 1)
                        .collect();
                    if paths.len() > MAX_COMPOSED {
                        paths.truncate(MAX_COMPOSED);
                        budget.trimmed = true;
                    }
                }
                Prefix::Unread { .. } => unread = true,
            }
        }
        for parts in paths {
            let path = match (!unread, route, parts) {
                (true, Some(route), Some(mut parts)) => {
                    parts.push(route.to_string());
                    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
                    Some(join_path(&parts))
                }
                _ => None,
            };
            let composed = Composed {
                path,
                edges: chain.edges.clone(),
                end: chain.end.clone(),
            };
            if out.contains(&composed) {
                continue;
            }
            if out.len() == MAX_COMPOSED {
                budget.trimmed = true;
                break;
            }
            out.push(composed);
        }
    }
    (out, budget.trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(line: u32) -> InstanceEnd {
        InstanceEnd {
            at: format!("src/app.ts:{line}"),
            end: InstanceAt::Root {
                file: "src/app.ts".to_string(),
                line,
                package: "fixture-server".to_string(),
                export: "default".to_string(),
                member: None,
                receiver: "instance:()".to_string(),
            },
        }
    }

    fn scope(values: Vec<String>, parents: Vec<InstanceEnd>) -> InstanceEnd {
        InstanceEnd {
            at: "src/routes.ts:3".to_string(),
            end: InstanceAt::Scope {
                file: "src/plugin.ts".to_string(),
                line: 5,
                member: "register".to_string(),
                prefix: Prefix::Stated { values },
                package: "fixture-server".to_string(),
                export: "default".to_string(),
                receiver: "instance:()".to_string(),
                parents,
            },
        }
    }

    /// carrick#2474 R2b: the chains and paths one site composes stop at
    /// [`MAX_COMPOSED`], and the site says the cap trimmed them: a scope
    /// with more stated values than the cap, and one under more parents.
    #[test]
    fn the_composed_paths_of_one_site_stop_at_the_cap() {
        let values: Vec<String> = (0..100).map(|n| format!("/v{n}")).collect();
        let none = Mounts::default();
        let (paths, trimmed) = compose(&[scope(values, vec![root(1)])], Some("/x"), &none);
        assert_eq!(paths.len(), MAX_COMPOSED);
        assert!(trimmed);
        let parents: Vec<InstanceEnd> = (1..=100).map(root).collect();
        let (paths, trimmed) =
            compose(&[scope(vec!["/a".to_string()], parents)], Some("/x"), &none);
        assert!(paths.len() <= MAX_COMPOSED, "{}", paths.len());
        assert!(trimmed);
        let (paths, trimmed) = compose(
            &[scope(
                vec!["/a".to_string(), "/b".to_string()],
                vec![root(1)],
            )],
            Some("/x"),
            &none,
        );
        assert_eq!(
            paths
                .iter()
                .map(|composed| composed.path.clone().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["/a/x", "/b/x"]
        );
        assert!(!trimmed);
    }
}
