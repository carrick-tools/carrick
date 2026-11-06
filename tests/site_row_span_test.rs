//! carrick#2604: a stated site row's type is asked at the payload, not at
//! the node enclosing a shifted span.
//!
//! The scanner's spans are SWC's (bytes from 1); the sidecar locates in
//! TypeScript positions (UTF-16 units from 0). The fixture is the
//! library-store one, with a comment of multi-byte characters before the
//! publish (`src/orders.ts`), scanned as the Action scans it. The real
//! sidecar answers; a shim in front of it records each `infer` request.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn sidecar_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js")
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git failed to spawn");
    assert!(status.success(), "git {args:?} failed");
}

/// A sidecar that forwards to the real one and appends every `infer`
/// request line to `<notes>/infer`.
fn shim(notes: &Path) -> PathBuf {
    let shim = notes.join("shim");
    fs::create_dir_all(shim.join("dist/src")).unwrap();
    fs::write(
        shim.join("dist/src/index.js"),
        format!(
            r#"
const fs = require('fs');
const path = require('path');
const readline = require('readline');
const {{ spawn }} = require('child_process');
const child = spawn(process.execPath, [{real:?}], {{ stdio: ['pipe', 'inherit', 'inherit'] }});
child.on('exit', (code) => process.exit(code ?? 1));
readline.createInterface({{ input: process.stdin, terminal: false }}).on('line', (line) => {{
  const request = JSON.parse(line);
  if (request.action === 'infer') fs.appendFileSync(path.join({notes:?}, 'infer'), line + '\n');
  child.stdin.write(line + '\n');
}}).on('close', () => child.stdin.end());
"#,
            notes = notes.display().to_string(),
            real = sidecar_script().display().to_string(),
        ),
    )
    .unwrap();
    fs::write(shim.join("package.json"), r#"{ "type": "commonjs" }"#).unwrap();
    shim
}

/// The scan's `infer` requests, and its stored blob.
struct Scan {
    infer: Vec<serde_json::Value>,
    blob: serde_json::Value,
}

fn scan(repo: &Path) -> Scan {
    let notes = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let shim = shim(notes.path());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", store.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_SIDECAR_DIR", &shim);
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let infer = fs::read_to_string(notes.path().join("infer"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let blob = fs::read_dir(store.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
        .expect("the scan stored a blob");
    Scan { infer, blob }
}

/// Characters of 2, 3 and 4 bytes in UTF-8, and 1, 1 and 2 in UTF-16.
const COMMENT: &str = "// caf\u{e9} \u{20ac} \u{1f600}\n";

/// UTF-16 units before byte `at` of `text`.
fn units_before(text: &str, at: usize) -> u32 {
    text[..at].encode_utf16().count() as u32
}

#[test]
fn a_site_rows_type_is_asked_at_its_payload_past_multibyte_text() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("relay");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library-store"),
        &repo,
    );
    fs::remove_file(repo.join("README.md")).ok();
    // The queue's declarations no longer verify its publish, so the scan
    // states no library row there and the site stands as a site row.
    fs::write(
        repo.join("node_modules/@fixture/queue/index.d.ts"),
        "export interface Bus {\n  publish(): Promise<void>;\n  subscribe(topic: string, handler: (payload: unknown) => Promise<void> | void): void;\n}\nexport declare const bus: Bus;\n",
    )
    .unwrap();
    let orders = repo.join("src/orders.ts");
    let source = format!("{COMMENT}{}", fs::read_to_string(&orders).unwrap());
    fs::write(&orders, &source).unwrap();
    let call = source.find("bus.publish(").unwrap();
    let call_end = call + source[call..].find(");").unwrap() + 1;
    let base = carrick::swc_scanner::SWC_SPAN_BASE as usize;
    let site_id = format!("src/orders.ts:{}-{}", call + base, call_end + base);
    let answer = serde_json::json!({
        "question_set": "qs-span",
        "answers": [{
            "site_id": site_id, "ok": true,
            "kind": {"value": "topic", "confidence": 0.95, "orders_agree": true},
            "role": null, "name": null, "method": null, "namespace": null,
            "name_argument": null, "payload_argument": null
        }]
    });
    fs::create_dir_all(repo.join("__llm__/site-questions")).unwrap();
    fs::write(
        repo.join("__llm__/site-questions/default.json"),
        answer.to_string(),
    )
    .unwrap();
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);

    let scanned = scan(&repo);
    let payload = call + source[call..].find("{ id }").unwrap();
    let (start, end) = (
        units_before(&source, payload),
        units_before(&source, payload + "{ id }".len()),
    );
    let asked: Vec<&serde_json::Value> = scanned
        .infer
        .iter()
        .flat_map(|request| request["requests"].as_array().unwrap())
        .filter(|request| {
            request["file_path"]
                .as_str()
                .unwrap()
                .ends_with("src/orders.ts")
                && request["infer_kind"] == "expression"
        })
        .collect();
    assert_eq!(
        asked.len(),
        1,
        "one expression request for the site row: {asked:?}"
    );
    assert_eq!(
        (
            asked[0]["span_start"].as_u64(),
            asked[0]["span_end"].as_u64()
        ),
        (Some(u64::from(start)), Some(u64::from(end))),
        "asked at the payload in TypeScript positions"
    );
    // The payload's own type, not the enclosing call's (`Promise<void>`) or
    // the function's.
    let alias = asked[0]["alias"].as_str().unwrap();
    let bundled = scanned.blob["bundled_types"].as_str().unwrap();
    let line = bundled
        .lines()
        .find(|line| line.contains(alias))
        .unwrap_or_else(|| panic!("{alias} is not in the bundle:\n{bundled}"));
    assert!(
        line.contains("id: string") && !line.contains("Promise"),
        "the site row's type is the payload's: {line}"
    );
}
