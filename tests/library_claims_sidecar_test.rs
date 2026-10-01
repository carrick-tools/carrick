//! `TypeSidecar::verify_library_claims` against the real sidecar (carrick#1659).
//!
//! The Rust protocol tests in `src/services/type_sidecar.rs` read hand-written
//! JSON, so a field renamed on one side of the wire pinned on carrick#1564
//! (comment 5937606126, section 3) would pass them. This test sends the
//! scanner's own structs to the built sidecar (`src/sidecar/dist`) and reads
//! the answer back, so the request schema and the response names are both
//! checked end to end. The package is invented and hand-written.

use carrick::services::type_sidecar::{
    BoundName, ClaimOn, ClaimSlot, KeyLabel, LibraryCheck, LibraryClaim, LibraryOp, LibraryRole,
    MakeForm, NameScope, NameScopeKind, OpName, SemanticsVerdict, TypeSidecar,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const QUEUE: &str = r#"export interface Job {
  id: string;
  trigger(payload: unknown): Promise<void>;
}
export interface JobOptions {
  id: string;
  description?: string;
  run: (payload: unknown) => Promise<void>;
}
export interface Channel {
  publish(message: unknown): Promise<void>;
  on(event: 'ready', listener: () => void): this;
}
export interface Queue {
  job(options: JobOptions): Job;
  tasks: { trigger(id: string, payload: unknown): Promise<void> };
  channel(name: string): Channel;
}
export declare const queue: Queue;
"#;

fn write(root: &Path, rel: &str, text: &str) {
    let file = root.join(rel);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

fn fixture(root: &Path) {
    write(
        root,
        "tsconfig.json",
        r#"{ "compilerOptions": { "target": "es2020", "module": "commonjs", "moduleResolution": "node", "strict": true, "skipLibCheck": true, "types": [] }, "include": ["src/**/*.ts"] }"#,
    );
    write(root, "package.json", r#"{ "name": "worker" }"#);
    write(root, "src/index.ts", "export const service = 1;\n");
    write(
        root,
        "node_modules/@fixture/queue/package.json",
        r#"{ "name": "@fixture/queue", "version": "2.4.1", "types": "index.d.ts" }"#,
    );
    write(root, "node_modules/@fixture/queue/index.d.ts", QUEUE);
}

fn real_sidecar(repo: &Path) -> TypeSidecar {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    assert!(
        entry.exists(),
        "build the sidecar first: cd src/sidecar && npm ci && npm run build"
    );
    let sidecar = TypeSidecar::spawn(&entry).expect("the sidecar spawns");
    sidecar.start_init(repo, None);
    sidecar
        .wait_ready(Duration::from_secs(120))
        .expect("the sidecar initialises on the fixture");
    sidecar
}

fn slot(arg: u32) -> ClaimSlot {
    ClaimSlot { arg, key: None }
}

/// A key of the options object at argument 0.
fn keyed(key: &str) -> ClaimSlot {
    ClaimSlot {
        arg: 0,
        key: Some(key.into()),
    }
}

fn check(claim_id: &str, receiver: &str, claim: LibraryClaim) -> LibraryCheck {
    LibraryCheck {
        claim_id: claim_id.into(),
        package: "@fixture/queue".into(),
        export: "queue".into(),
        role: LibraryRole::Broker,
        receiver: receiver.into(),
        claim,
    }
}

/// The definition maker `queue.job({ id, description?, run })`.
fn job(labels: &[(&str, KeyLabel)]) -> LibraryClaim {
    LibraryClaim::Make {
        form: MakeForm::Call,
        member: Some("job".into()),
        base: None,
        prefix: None,
        name: Some(keyed("id")),
        handler: Some(keyed("run")),
        key_labels: labels
            .iter()
            .map(|(key, label)| (key.to_string(), *label))
            .collect(),
        name_scope: Some(NameScope {
            scope: NameScopeKind::Service,
            namespace: None,
        }),
        picker: Some("model/q1".into()),
    }
}

/// Which receivers an element applies to: every one `on` names, or exactly
/// the one `of` names.
enum Place {
    On(ClaimOn),
    Of(&'static str),
}

fn placed(place: Place) -> (Option<ClaimOn>, Option<String>) {
    match place {
        Place::On(on) => (Some(on), None),
        Place::Of(of) => (None, Some(of.into())),
    }
}

/// A send whose payload is argument `payload`.
fn send(member: &str, path: &[&str], place: Place, name: OpName, payload: u32) -> LibraryClaim {
    let (on, of) = placed(place);
    LibraryClaim::Op {
        op: LibraryOp::Send,
        member: Some(member.into()),
        path: path.iter().map(|hop| hop.to_string()).collect(),
        on,
        of,
        name: Some(name),
        payload: Some(slot(payload)),
        handler: None,
        ack: None,
        key_labels: BTreeMap::new(),
        name_scope: Some(NameScope {
            scope: NameScopeKind::Global,
            namespace: Some("task".into()),
        }),
        picker: None,
        options: None,
        method: None,
        method_key: None,
    }
}

fn verdicts(answer: &[carrick::services::type_sidecar::SemanticsResult]) -> Vec<String> {
    answer
        .iter()
        .map(|result| match (&result.verdict, &result.reason) {
            (SemanticsVerdict::Verified, _) => format!("{} verified", result.claim_id),
            (verdict, reason) => format!(
                "{} {:?} {}",
                result.claim_id,
                verdict,
                reason.as_deref().unwrap_or("")
            ),
        })
        .collect()
}

#[test]
fn the_scanner_structs_round_trip_through_the_real_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().canonicalize().unwrap();
    fixture(&repo);
    let sidecar = real_sidecar(&repo);

    let bound = |bound| OpName::Bound { bound };
    let checks = vec![
        check(
            "job",
            "export",
            job(&[("id", KeyLabel::Name), ("description", KeyLabel::NotName)]),
        ),
        check(
            "job.trigger",
            "instance:job",
            send(
                "trigger",
                &[],
                Place::Of("instance:job"),
                bound(BoundName::Maker),
                0,
            ),
        ),
        check(
            "tasks.trigger",
            "export",
            send(
                "trigger",
                &["tasks"],
                Place::On(ClaimOn::Export),
                OpName::Slot(slot(0)),
                1,
            ),
        ),
        check(
            "channel",
            "export",
            LibraryClaim::Scope {
                member: "channel".into(),
                name: slot(0),
                path: vec![],
                on: Some(ClaimOn::Export),
                of: None,
                key_labels: BTreeMap::new(),
                name_scope: None,
                picker: None,
            },
        ),
        check(
            "channel.publish",
            "export>scope:channel",
            send(
                "publish",
                &[],
                Place::Of("export>scope:channel"),
                bound(BoundName::Scope),
                0,
            ),
        ),
        check(
            "channel.ready",
            "export>scope:channel",
            LibraryClaim::Reserved {
                member: "on".into(),
                name: "ready".into(),
                path: vec![],
                on: None,
                of: Some("export>scope:channel".into()),
                picker: None,
            },
        ),
        // Claimed for instances, asked on the export.
        check(
            "misplaced",
            "export",
            send(
                "trigger",
                &["tasks"],
                Place::On(ClaimOn::Instance),
                OpName::Slot(slot(0)),
                1,
            ),
        ),
        // `on` and `of` together name no one receiver.
        check(
            "both",
            "export",
            LibraryClaim::Op {
                op: LibraryOp::Send,
                member: Some("trigger".into()),
                path: vec!["tasks".into()],
                on: Some(ClaimOn::Both),
                of: Some("instance:job".into()),
                name: Some(OpName::Slot(slot(0))),
                payload: Some(slot(1)),
                handler: None,
                ack: None,
                key_labels: BTreeMap::new(),
                name_scope: None,
                picker: None,
                options: None,
                method: None,
                method_key: None,
            },
        ),
    ];
    let answer = sidecar
        .verify_library_claims(&repo, &checks)
        .expect("the sidecar answers");
    assert_eq!(
        verdicts(&answer.verdicts),
        vec![
            "job verified",
            "job.trigger verified",
            "tasks.trigger verified",
            "channel verified",
            "channel.publish verified",
            "channel.ready verified",
            "misplaced Unchecked receiver_invalid",
            "both Unchecked claim_invalid",
        ]
    );
    assert_eq!(answer.modules.len(), 1);
    assert_eq!(answer.modules[0].package, "@fixture/queue");
    assert_eq!(
        answer.modules[0].installed_version.as_deref(),
        Some("2.4.1")
    );
    assert!(answer.duration_ms.is_some(), "duration_ms is answered");

    // Strict D2: an unlabelled optional description beside the id leaves the
    // name ambiguous, so the maker builds no instance its sends can be read on.
    let unlabelled = vec![check("job", "export", job(&[])), checks[1].clone()];
    let answer = sidecar
        .verify_library_claims(&repo, &unlabelled)
        .expect("the sidecar answers");
    assert_eq!(
        verdicts(&answer.verdicts),
        vec![
            "job Failed name_ambiguous",
            "job.trigger Unchecked maker_unverified",
        ]
    );
}
