//! Call edges through path aliases (carrick#1104).
//!
//! Drives the real scanner binary offline over the two variants in
//! `tests/fixtures/alias-import-callers/`: a Deno workspace whose member maps
//! `@/` in its import map, and its Node twin, where the same alias comes from a
//! tsconfig `paths` entry one `extends` hop away and `queueCourierPickup` is
//! imported through a package.json `#pickup/*` subpath import instead. Reads
//! `function_definitions[].calls` from the WRITTEN BLOB, because that is the
//! field `get_callers` inverts.
//!
//! Pre-fix baseline, measured on the 0.3.70 binary: `checkSlotAvailability` had
//! 3 of its 6 callers (only the relative controls) and `queueCourierPickup` had
//! 0 of 1, in both variants. Every alias row below FAILS on that binary.
//!
//! The singleton rows (carrick#1147) FAIL on the 0.3.72 binary in both
//! variants, the relative control included: an imported module-scope instance
//! resolved to nothing whatever its specifier.
//!
//! The Node twin also imports `queueCourierPickup` through `~/`, an alias set
//! only in `vite.config.ts`. No config the scanner reads declares it, so that
//! call must record no edge AND be reported on stderr. The answer key is the
//! compiler's: `ts.resolveModuleName` with the fixture's tsconfig resolves
//! `@/…` and `#pickup/…` and resolves `~/…` to nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir(variant: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/alias-import-callers")
        .join(variant)
}

struct Scan {
    /// Caller key -> the callee names its edges record.
    calls: BTreeMap<String, BTreeSet<String>>,
    stderr: String,
}

impl Scan {
    /// Every caller with an edge to `callee`: what `get_callers` returns.
    fn callers_of(&self, callee: &str) -> BTreeSet<String> {
        self.calls
            .iter()
            .filter(|(_, callees)| callees.contains(callee))
            .map(|(caller, _)| caller.clone())
            .collect()
    }
}

fn scan(variant: &str) -> Scan {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    // No cassettes: every analyzer call answers empty, and call edges are
    // deterministic, so none are needed.
    let cassettes = tempfile::tempdir().expect("temp cassette dir");

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir(variant))
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.path().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // The assertion is on call edges; the type layer is not under test
        // and a Deno member with an npm import has no prepared install here.
        .env("CARRICK_ALLOW_MISSING_TYPES", "1");
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "{variant} fixture scan exited non-zero:\n{stderr}"
    );

    let mut blobs = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    blobs.sort();
    assert_eq!(blobs.len(), 1, "expected one written blob, got {blobs:?}");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&blobs[0]).expect("read blob")).expect("parse blob");

    let calls = blob["function_definitions"]
        .as_object()
        .expect("blob has no function_definitions")
        .iter()
        .map(|(key, def)| {
            let callees = def["calls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|call| call["name"].as_str().map(str::to_owned))
                .collect();
            (key.clone(), callees)
        })
        .collect();
    Scan { calls, stderr }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The answer key shared by both variants: three relative controls and three
/// alias callers of `checkSlotAvailability`, and the one caller of
/// `queueCourierPickup`.
fn assert_answer_key(scan: &Scan) {
    assert_eq!(
        scan.callers_of("checkSlotAvailability"),
        set(&[
            // Relative controls, resolved before the fix too.
            "ReservationService.reserve",
            "auditLocker",
            "transaction_handler@apps/lockers/src/slots/reservation.service.ts",
            // Through the alias.
            "DeliveryService.plan",
            "acceptReturn",
            "transaction_handler@apps/lockers/src/deliveries/delivery.service.ts",
        ])
    );
    assert_eq!(
        scan.callers_of("queueCourierPickup"),
        set(&["EventProcessorService.handle"])
    );
    // `db` imported through `@/db.ts` resolves like the relative `../db.ts`.
    assert_eq!(
        scan.callers_of("db.transaction"),
        set(&[
            "DeliveryService.confirm",
            "ReservationService.reserveInTransaction"
        ])
    );
    // A module-scope instance of a class imported through `@/`.
    assert_eq!(
        scan.callers_of("DeliveryService.plan"),
        set(&["get_lockers__id_plan_handler"])
    );
    // An exported singleton (`export const notifier = new LockerNotifier()`)
    // whose class is imported into the module that constructs it, called
    // through a relative import and through the alias (carrick#1147).
    assert_eq!(
        scan.callers_of("LockerNotifier.notifyReady"),
        set(&["announcePickupReady", "announceReturnReady"])
    );
}

#[test]
fn deno_import_map_aliases_record_caller_edges() {
    let scan = scan("deno");
    assert_answer_key(&scan);
    // `hono` maps to a registry package: an import, never an edge.
    assert!(
        scan.calls
            .values()
            .all(|callees| !callees.iter().any(|c| c.contains("Hono"))),
        "an npm import-map entry must not resolve to a file"
    );
}

/// The Node twin, scanned once for the two tests that read it.
fn tsconfig_scan() -> &'static Scan {
    static SCAN: std::sync::OnceLock<Scan> = std::sync::OnceLock::new();
    SCAN.get_or_init(|| scan("tsconfig"))
}

#[test]
fn tsconfig_paths_and_subpath_imports_record_caller_edges() {
    assert_answer_key(tsconfig_scan());
}

#[test]
fn a_bundler_only_alias_records_no_edge_and_is_reported() {
    let scan = tsconfig_scan();
    assert_eq!(
        scan.calls.get("legacyPickup"),
        Some(&BTreeSet::new()),
        "`~/` is declared only in vite.config.ts"
    );
    // Found by what the line is about, not by a ticket ref: the ref was
    // dropped when the line was cut to what a reader acts on (carrick#1273).
    let report = scan
        .stderr
        .lines()
        .find(|line| line.contains("undeclared alias(es)"))
        .unwrap_or_else(|| panic!("no unresolved-alias report on stderr:\n{}", scan.stderr));
    assert!(
        report.contains(
            "1 import(s) through 1 undeclared alias(es) are unresolved: \
                         ~/pickup/queue.ts"
        ),
        "report must count and name the specifier: {report}"
    );
    assert!(
        report.ends_with("Declare them in tsconfig, package.json or a Deno import map."),
        "and say what to do about it: {report}"
    );
}
