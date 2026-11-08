//! The scanner stages of a seeded PR (carrick-cloud#2880), computed from the
//! PR result payload.
//!
//! A PR check row passes through five stages: `found` (both sides of the
//! operation are named), `paired` (they are paired), `judged` (the pair's
//! types are compared), `finding` (the payload holds the row, marked as the
//! PR's), `shown` (the cloud's renderer posts it). This module computes the
//! first four, the stages the scanner owns, and holds them to the seed's
//! declared `known_miss.stage`. The cloud's PR-accuracy suite computes all
//! five with the same rule, so the two evaluators agree on one table.
//!
//! The rule, per seed. Targets are every non-optional `expect.rows` entry
//! plus the check itself. A target's first wrong stage is the first stage of
//! its family it does not pass:
//!
//! - type family (`risk_introduced`, `risk_could_not_compare` with a
//!   `type_mismatch`): found, paired, judged, finding;
//! - routing family (the same rows with a `method_mismatch`): finding only
//!   (a verb-mismatched producer is in neither `verified` nor `orphaned`, and
//!   the verdict is a routing fact);
//! - break family (`removed_still_called`): found, paired, finding;
//! - delta family (`delta_new`, `delta_removed`): finding;
//! - the check: finding only; the payload holds a raiser iff the correct
//!   title is not clean.
//!
//! In a closed-world seed (not `open`) an unexpected raiser or delta row is
//! wrong at `finding`. The seed's stage is the earliest wrong stage over all
//! targets and false rows, or none. Paths match through
//! `carrick_match::paths_match`, so `:encoded` and `:orderId` are one route.

#![allow(dead_code)]

use serde_json::Value;

/// The stages, in the order a row passes them.
pub const STAGES: [&str; 5] = ["found", "paired", "judged", "finding", "shown"];

/// Index of `shown`: the first stage the scanner does not own.
const SHOWN: usize = 4;

/// The kinds of change a seed makes (the table's second key).
pub const CHANGES: [&str; 6] = [
    "shape_break",
    "verb_change",
    "removed_while_used",
    "sender_renamed",
    "delta_only",
    "quiet",
];

/// Titles of a check that has nothing to act on.
const CLEAN_TITLES: [&str; 3] = [
    "no_contract_changes",
    "no_contract_risks",
    "no_endpoint_changes",
];

fn stage_index(stage: &str) -> Option<usize> {
    STAGES.iter().position(|s| *s == stage)
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn truthy(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false))
}

/// `METHOD path` as the manifest labels an operation.
fn split_label(label: &str) -> (&str, &str) {
    label
        .split_once(' ')
        .unwrap_or_else(|| panic!("label {label:?} has no method"))
}

/// Whether the operation `method path` is the one `label` names.
fn names(label: &str, method: &Value, path: &Value) -> bool {
    let (want_method, want_path) = split_label(label);
    names_parts(want_method, want_path, method, path)
}

fn names_parts(want_method: &str, want_path: &str, method: &Value, path: &Value) -> bool {
    method
        .as_str()
        .is_some_and(|m| m.eq_ignore_ascii_case(want_method))
        && path
            .as_str()
            .is_some_and(|p| carrick_match::paths_match(want_path, p))
}

fn call_sites(finding: &Value) -> Vec<&str> {
    array(&finding["call_sites"])
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

/// Whether a finding is marked as the PR's: main did not have it
/// (`on_main: false`), or the run could not tell (`on_main_unknown`).
fn marked(finding: &Value) -> bool {
    finding["on_main"] == false || truthy(&finding["on_main_unknown"])
}

fn at_site(finding: &Value, site: Option<&str>) -> bool {
    site.is_none_or(|site| call_sites(finding).contains(&site))
}

/// What makes a correct check not clean: an introduced (or not compared)
/// type or method risk, a break row the PR introduced, or a file whose
/// operation changes the delta could not list (`delta.files_not_read`,
/// carrick#3233).
fn raisers(payload: &Value) -> Vec<&Value> {
    array(&payload["findings"])
        .iter()
        .filter(|f| match f["kind"].as_str() {
            Some("type_mismatch" | "method_mismatch") => marked(f),
            Some("missing_endpoint" | "orphaned_endpoint") => truthy(&f["removed"]) && marked(f),
            _ => false,
        })
        .chain(array(&payload["delta"]["files_not_read"]))
        .collect()
}

/// The first of found/paired/judged/finding a type or routing row fails.
fn risk_row(label: &str, site: Option<&str>, payload: &Value) -> Option<(usize, String)> {
    let findings = array(&payload["findings"]);
    let verified: Vec<&Value> = array(&payload["verified"])
        .iter()
        .filter(|v| names(label, &v["method"], &v["path"]))
        .collect();
    let routing = findings.iter().find(|f| {
        f["kind"] == "method_mismatch" && names(label, &f["method"], &f["path"]) && at_site(f, site)
    });
    if let Some(f) = routing {
        return (!marked(f)).then(|| (3, format!("{label}: method_mismatch is on main")));
    }
    let delta_new = array(&payload["delta"]["new_endpoints"])
        .iter()
        .any(|e| names(label, &e["method"], &e["path"]));
    let producer = !verified.is_empty()
        || delta_new
        || findings
            .iter()
            .any(|f| f["kind"] == "orphaned_endpoint" && names(label, &f["method"], &f["path"]));
    let consumer = !verified.is_empty()
        || findings.iter().any(|f| {
            f["kind"] == "missing_endpoint"
                && names(label, &f["method"], &f["path"])
                && at_site(f, site)
        });
    if !(producer && consumer) {
        return Some((
            0,
            format!("{label}: producer found {producer}, consumer found {consumer}"),
        ));
    }
    if verified.is_empty() {
        return Some((1, format!("{label}: no verified row")));
    }
    if !verified.iter().any(|v| v["type_verdict"] == "incompatible") {
        return Some((
            2,
            format!(
                "{label}: type_verdict {}",
                verified[0]["type_verdict"].as_str().unwrap_or("absent")
            ),
        ));
    }
    let mismatch = findings.iter().find(|f| {
        f["kind"] == "type_mismatch" && names(label, &f["method"], &f["path"]) && at_site(f, site)
    });
    match mismatch {
        Some(f) if marked(f) => None,
        Some(_) => Some((3, format!("{label}: type_mismatch is on main"))),
        None => Some((3, format!("{label}: no type_mismatch at {site:?}"))),
    }
}

fn delta_row(kind: &str, label: &str, payload: &Value) -> Option<(usize, String)> {
    let side = if kind == "delta_new" {
        "new_endpoints"
    } else {
        "removed_endpoints"
    };
    let hit = array(&payload["delta"][side])
        .iter()
        .any(|e| names(label, &e["method"], &e["path"]));
    (!hit).then(|| (3, format!("{kind} {label}: no delta.{side} row")))
}

fn still_called_row(label: &str, payload: &Value) -> Option<(usize, String)> {
    let removed = array(&payload["delta"]["removed_endpoints"])
        .iter()
        .any(|e| names(label, &e["method"], &e["path"]));
    if !removed {
        return Some((0, format!("{label}: no removed row")));
    }
    let break_row = array(&payload["findings"])
        .iter()
        .find(|f| names(label, &f["removed"]["method"], &f["removed"]["path"]));
    match break_row {
        None => Some((1, format!("{label}: no break row"))),
        Some(f) if !marked(f) => Some((3, format!("{label}: break row is on main"))),
        Some(_) => None,
    }
}

/// Every wrong scanner stage of seed `entry` against its payload, each with
/// what is wrong. Empty: the scanner stages are all right.
pub fn wrong_scanner_stages(entry: &Value, payload: &Value) -> Vec<(usize, String)> {
    let mut wrong = Vec::new();
    let expect = &entry["expect"];
    let rows = array(&expect["rows"]);
    for row in rows.iter().filter(|r| r["optional"] != true) {
        let label = row["label"].as_str().unwrap_or_default();
        let site = row["site"].as_str();
        let found = match row["kind"].as_str().unwrap_or_default() {
            "risk_introduced" | "risk_could_not_compare" => risk_row(label, site, payload),
            kind @ ("delta_new" | "delta_removed") => delta_row(kind, label, payload),
            "removed_still_called" => still_called_row(label, payload),
            _ => None,
        };
        wrong.extend(found);
    }
    let clean = expect["title"]
        .as_str()
        .is_some_and(|title| CLEAN_TITLES.contains(&title));
    let raised = raisers(payload);
    if clean != raised.is_empty() {
        wrong.push((
            3,
            format!(
                "check: title {} but {} raiser(s)",
                expect["title"],
                raised.len()
            ),
        ));
    }
    if expect["open"] != true {
        wrong.extend(false_rows(rows, payload));
    }
    wrong
}

/// Rows the payload carries that the correct check does not (closed world).
fn false_rows(rows: &[Value], payload: &Value) -> Vec<(usize, String)> {
    let expected = |kind: &str, m: &Value, p: &Value| {
        rows.iter()
            .any(|r| r["kind"] == kind && names(r["label"].as_str().unwrap_or(" "), m, p))
    };
    let mut false_rows = Vec::new();
    for e in array(&payload["delta"]["new_endpoints"]) {
        if !expected("delta_new", &e["method"], &e["path"]) {
            false_rows.push((3, format!("unexpected new endpoint {e}")));
        }
    }
    let missing: Vec<&Value> = array(&payload["findings"])
        .iter()
        .filter(|f| f["kind"] == "missing_endpoint")
        .collect();
    for e in array(&payload["delta"]["removed_endpoints"]) {
        if !expected("delta_removed", &e["method"], &e["path"]) {
            false_rows.push((3, format!("unexpected removed endpoint {e}")));
        }
        let (m, p) = (&e["method"], &e["path"]);
        let still_called = missing.iter().any(|f| {
            names_parts(
                m.as_str().unwrap_or_default(),
                p.as_str().unwrap_or_default(),
                &f["method"],
                &f["path"],
            )
        });
        if still_called && !expected("removed_still_called", m, p) {
            false_rows.push((3, format!("unexpected still-called row {e}")));
        }
    }
    for f in array(&payload["findings"]) {
        let risk = matches!(
            f["kind"].as_str(),
            Some("type_mismatch" | "method_mismatch")
        );
        if !risk
            || f["edge_source"] == "candidate"
            || f["verdict_state"] == "unresolved"
            || f["on_main"] == true
        {
            continue;
        }
        let site = call_sites(f).first().copied();
        let covered = rows.iter().any(|r| {
            matches!(
                r["kind"].as_str(),
                Some("risk_introduced" | "risk_could_not_compare")
            ) && names(r["label"].as_str().unwrap_or(" "), &f["method"], &f["path"])
                && r["site"].as_str() == site
        });
        if !covered {
            false_rows.push((
                3,
                format!(
                    "unexpected risk row {} {} at {site:?}",
                    f["method"], f["path"]
                ),
            ));
        }
    }
    false_rows
}

/// The stage `known_miss` declares, or none for a seed with no miss.
pub fn declared_stage(entry: &Value) -> Option<usize> {
    entry["known_miss"]["stage"]
        .as_str()
        .map(|s| stage_index(s).unwrap_or_else(|| panic!("unknown stage {s:?}")))
}

/// Hold seed `name` to its declaration, in both directions. A declaration of
/// `shown` or none asks the scanner stages to be all right; any other stage
/// is the one the run must stop at.
pub fn assert_declared_stage(name: &str, entry: &Value, payload: &Value) {
    let wrong = wrong_scanner_stages(entry, payload);
    let computed = wrong.iter().map(|(stage, _)| *stage).min();
    let declared = declared_stage(entry).filter(|stage| *stage < SHOWN);
    let describe =
        |stage: Option<usize>| stage.map_or("none (scanner stages all right)", |s| STAGES[s]);
    match (declared, computed) {
        (None, None) => {}
        (Some(d), Some(c)) if d == c => {}
        (declared, Some(c)) if declared.is_none_or(|d| c < d) => panic!(
            "{name}: the scanner stops at {} but known_miss declares {}; wrong: {wrong:#?}",
            STAGES[c],
            describe(declared)
        ),
        (declared, computed) => panic!(
            "{name}: stale known_miss: it declares {} and the scanner stops at {}; move or drop \
             the declaration",
            describe(declared),
            describe(computed)
        ),
    }
}

/// Why seed `seed`'s manifest entry is malformed, or none. `change` is from
/// the closed list; `known_miss` is `{stage, issues}` and nothing else;
/// `retired` is `{date, reason}` and nothing else; there is no status key.
pub fn seed_shape_problem(seed: &Value) -> Option<String> {
    let name = seed["name"].as_str().unwrap_or("?");
    let object = seed.as_object()?;
    if object.contains_key("status") {
        return Some(format!(
            "{name}: a seed carries no status; the table computes it"
        ));
    }
    if !seed["change"]
        .as_str()
        .is_some_and(|c| CHANGES.contains(&c))
    {
        return Some(format!(
            "{name}: change {} is not in {CHANGES:?}",
            seed["change"]
        ));
    }
    if let Some(miss) = object.get("known_miss") {
        let Some(keys) = miss.as_object() else {
            return Some(format!("{name}: known_miss is not an object: {miss}"));
        };
        if keys.len() != 2 || !keys.contains_key("stage") || !keys.contains_key("issues") {
            return Some(format!(
                "{name}: known_miss keys are exactly stage and issues"
            ));
        }
        if !miss["stage"]
            .as_str()
            .is_some_and(|s| stage_index(s).is_some())
        {
            return Some(format!(
                "{name}: known_miss.stage {} is not in {STAGES:?}",
                miss["stage"]
            ));
        }
        let issues = array(&miss["issues"]);
        let issue_ok = |i: &Value| {
            i.as_str().is_some_and(|i| {
                let number = i
                    .strip_prefix("carrick#")
                    .or_else(|| i.strip_prefix("carrick-cloud#"));
                number.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        };
        if issues.is_empty() || !issues.iter().all(issue_ok) {
            return Some(format!(
                "{name}: known_miss.issues are one or more carrick#N or carrick-cloud#N"
            ));
        }
    }
    if let Some(retired) = object.get("retired") {
        let text = |key: &str| retired[key].as_str().is_some_and(|s| !s.trim().is_empty());
        let keys = retired.as_object().map_or(0, |o| o.len());
        if keys != 2 || !text("date") || !text("reason") {
            return Some(format!(
                "{name}: retired is exactly {{date, reason}}, both text"
            ));
        }
    }
    None
}

/// Whether the seed takes part in runs.
pub fn is_retired(seed: &Value) -> bool {
    seed.get("retired").is_some()
}

/// The seeds some non-ignored test of `source` (the text of
/// `pr_on_main_test.rs`) runs through `manifest_seed_payload`. A helper
/// function counts for the seed it runs; an `#[ignore]`d function counts for
/// none.
pub fn seeds_with_a_running_test(source: &str, names: &[String]) -> Vec<String> {
    let mut ignored = false;
    let mut pending_ignore = false;
    let mut covered = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if line.starts_with("#[") {
            pending_ignore |= trimmed.starts_with("#[ignore");
        } else if line.starts_with("fn ")
            || line.starts_with("async fn ")
            || line.starts_with("pub ")
        {
            ignored = pending_ignore;
            pending_ignore = false;
        } else if !line.starts_with("///") && !line.trim().is_empty() && !line.starts_with(' ') {
            pending_ignore = false;
        }
        if ignored {
            continue;
        }
        for name in names {
            let call = ["manifest_seed_payload(", "\"", name, "\"", ")"].concat();
            if line.contains(&call) && !covered.contains(name) {
                covered.push(name.clone());
            }
        }
    }
    covered
}
