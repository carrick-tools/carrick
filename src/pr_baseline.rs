//! Which of a PR run's findings were already on main (carrick-cloud#1369).
//!
//! A PR check fails only on the risks the PR introduced. The cloud cannot tell
//! which those are: the PR payload carries display labels and no pairing, and
//! main's method mismatches are stored nowhere (carrick-cloud#1370). The
//! scanner can. On a PR run it holds main's copy of this repo (its last
//! uploaded index) beside the peers, so it matches that copy against the same
//! peers and marks each PR finding with whether main yields it too.
//!
//! "The same finding" is the same kind on the same pairing
//! ([`Finding::pair`]): producer service and operation, consumer service and
//! file, and the direction. The line is left out, because a PR that edits the
//! consumer file above a call moves every line below the edit.
//!
//! Leaving the line out would let a second broken call slip through as "on
//! main" when it sits in the same file and hits the same pairing as an
//! existing one. So the comparison counts call sites per file: when the PR
//! has more sites for a pairing in one file than main has, every finding with
//! sites in that group reads as introduced. That errs toward failing the
//! check, never toward hiding a break.

use std::collections::HashMap;

use crate::findings::Finding;

/// A comparison bucket: kind, pairing, and the consumer file of one site.
type SiteGroup = (&'static str, String, String);

/// The file half of a `file:line` (or `file:line:col`, or bare `file`) site.
fn site_file(site: &str) -> String {
    crate::type_manifest::parse_file_location(site).0
}

/// The buckets one finding counts in, one entry per call site. A finding with
/// no pairing counts in none, and a finding with a pairing but no sites
/// counts once, under an empty file.
fn groups(finding: &Finding) -> Vec<SiteGroup> {
    let Some(pair) = finding.pair() else {
        return Vec::new();
    };
    let sites: &[String] = match finding {
        Finding::TypeMismatch { call_sites, .. } | Finding::MethodMismatch { call_sites, .. } => {
            call_sites
        }
        _ => return Vec::new(),
    };
    if sites.is_empty() {
        return vec![(finding.kind(), pair.to_string(), String::new())];
    }
    sites
        .iter()
        .map(|site| (finding.kind(), pair.to_string(), site_file(site)))
        .collect()
}

fn count(findings: &[Finding]) -> HashMap<SiteGroup, usize> {
    let mut counts = HashMap::new();
    for finding in findings {
        for group in groups(finding) {
            *counts.entry(group).or_insert(0) += 1;
        }
    }
    counts
}

/// What main's copy could be compared on. A type mismatch can only be judged
/// "on main" when main's type check ran; otherwise main shows no type
/// findings at all and every PR type mismatch would read as introduced for a
/// reason that has nothing to do with the PR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Baseline {
    pub types_compared: bool,
}

/// Mark each pairing-carrying finding in `pr` with whether `main` has it.
///
/// A finding is `on_main: true` when, for every site group it counts in, main
/// has at least as many sites as the PR. A finding with no pairing, or a type
/// mismatch when main's type check did not run, is left unmarked, which the
/// cloud reads as today's behaviour.
pub fn mark_on_main(pr: &mut [Finding], main: &[Finding], baseline: Baseline) {
    let main_counts = count(main);
    let pr_counts = count(pr);
    for finding in pr.iter_mut() {
        let groups = groups(finding);
        if groups.is_empty() {
            continue;
        }
        if matches!(finding, Finding::TypeMismatch { .. }) && !baseline.types_compared {
            continue;
        }
        let on_main = groups.iter().all(|group| {
            let ours = pr_counts.get(group).copied().unwrap_or(0);
            let theirs = main_counts.get(group).copied().unwrap_or(0);
            ours <= theirs
        });
        finding.set_on_main(Some(on_main));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::ManifestTypeKind;

    const TYPES: Baseline = Baseline {
        types_compared: true,
    };

    fn type_mismatch(pair: &str, site: &str) -> Finding {
        Finding::type_mismatch(
            "POST",
            "/orders",
            None,
            vec![site.to_string()],
            "Order",
            "OrderInput",
            "Property 'total' is missing",
        )
        .with_direction(Some(ManifestTypeKind::Request))
        .with_pair(Some(pair.to_string()))
    }

    fn method_mismatch(pair: &str, sites: &[&str]) -> Finding {
        Finding::method_mismatch(
            "PUT",
            "/orders/:id",
            None,
            sites.iter().map(|s| s.to_string()).collect(),
            "POST",
        )
        .with_pair(Some(pair.to_string()))
    }

    fn on_main(finding: &Finding) -> Option<bool> {
        match finding {
            Finding::TypeMismatch { on_main, .. } | Finding::MethodMismatch { on_main, .. } => {
                *on_main
            }
            _ => None,
        }
    }

    const ORDERS: &str = "orders-api|POST|/orders~web|src/checkout.ts|request";

    /// The smoke-test PR's shape: it edited the consumer file above an
    /// existing broken call, so the line moved. The pairing did not.
    #[test]
    fn a_finding_whose_line_moved_is_on_main() {
        let main = vec![type_mismatch(ORDERS, "src/checkout.ts:40")];
        let mut pr = vec![type_mismatch(ORDERS, "src/checkout.ts:52")];
        mark_on_main(&mut pr, &main, TYPES);
        assert_eq!(on_main(&pr[0]), Some(true));
    }

    #[test]
    fn a_pairing_main_does_not_have_is_introduced() {
        let main = vec![type_mismatch(ORDERS, "src/checkout.ts:40")];
        let other = "orders-api|POST|/orders~web|src/cart.ts|request";
        let mut pr = vec![
            type_mismatch(ORDERS, "src/checkout.ts:40"),
            type_mismatch(other, "src/cart.ts:9"),
        ];
        mark_on_main(&mut pr, &main, TYPES);
        assert_eq!(on_main(&pr[0]), Some(true));
        assert_eq!(on_main(&pr[1]), Some(false));
    }

    /// The same kind is part of the identity: a method mismatch on main does
    /// not cover a type mismatch that happens to carry the same key.
    #[test]
    fn the_kind_is_part_of_the_identity() {
        let main = vec![method_mismatch(ORDERS, &["src/checkout.ts:40"])];
        let mut pr = vec![type_mismatch(ORDERS, "src/checkout.ts:40")];
        mark_on_main(&mut pr, &main, TYPES);
        assert_eq!(on_main(&pr[0]), Some(false));
    }

    /// A second broken call on the same pairing in the same file cannot be
    /// told apart from the first once lines are dropped, so both read as
    /// introduced rather than letting the new one pass.
    #[test]
    fn a_second_broken_call_in_the_same_file_fails_the_whole_group() {
        let main = vec![type_mismatch(ORDERS, "src/checkout.ts:40")];
        let mut pr = vec![
            type_mismatch(ORDERS, "src/checkout.ts:40"),
            type_mismatch(ORDERS, "src/checkout.ts:88"),
        ];
        mark_on_main(&mut pr, &main, TYPES);
        assert_eq!(on_main(&pr[0]), Some(false));
        assert_eq!(on_main(&pr[1]), Some(false));
    }

    /// A method mismatch aggregates every consumer site of one wrong verb, so
    /// a new file sending it makes the finding introduced.
    #[test]
    fn a_method_mismatch_that_gained_a_file_is_introduced() {
        let pair = "POST /orders/:id~PUT";
        let main = vec![method_mismatch(pair, &["src/a.ts:3"])];
        let mut same = vec![method_mismatch(pair, &["src/a.ts:7"])];
        mark_on_main(&mut same, &main, TYPES);
        assert_eq!(on_main(&same[0]), Some(true));

        let mut grew = vec![method_mismatch(pair, &["src/a.ts:7", "src/b.ts:1"])];
        mark_on_main(&mut grew, &main, TYPES);
        assert_eq!(on_main(&grew[0]), Some(false));
    }

    #[test]
    fn a_finding_without_a_pairing_is_left_unmarked() {
        let main = vec![type_mismatch(ORDERS, "src/checkout.ts:40")];
        let mut pr = vec![
            Finding::type_mismatch("POST", "/orders", None, vec![], "A", "B", "x"),
            Finding::missing_endpoint("GET", "/gone", None, vec!["src/x.ts:1".into()]),
        ];
        mark_on_main(&mut pr, &main, TYPES);
        assert_eq!(on_main(&pr[0]), None);
        assert!(
            !serde_json::to_value(&pr[1])
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("on_main")
        );
    }

    /// Main's type check did not run, so main has no type findings to compare
    /// with; the PR's type mismatches keep today's behaviour. Method
    /// mismatches need no type check and are still compared.
    #[test]
    fn type_mismatches_stay_unmarked_when_mains_type_check_did_not_run() {
        let pair = "POST /orders/:id~PUT";
        let main = vec![method_mismatch(pair, &["src/a.ts:3"])];
        let mut pr = vec![
            type_mismatch(ORDERS, "src/checkout.ts:40"),
            method_mismatch(pair, &["src/a.ts:3"]),
        ];
        mark_on_main(
            &mut pr,
            &main,
            Baseline {
                types_compared: false,
            },
        );
        assert_eq!(on_main(&pr[0]), None);
        assert_eq!(on_main(&pr[1]), Some(true));
    }
}
