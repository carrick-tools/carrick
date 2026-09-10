//! Real scan -> incremental replay -> local read projection, with synthetic
//! authenticated cache data and a refused loopback proxy. No remote network,
//! model, upload or developer credential is used.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

const TOKEN: &str = "synthetic-hosted-reader";

fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}

fn run(root: &Path, args: &[&str], token: Option<&str>) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_carrick"));
    command
        .args(args)
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join("empty-config"))
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("https_proxy", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .env_remove("CARRICK_LOCAL_STORAGE_DIR")
        .env_remove("CARRICK_LOCAL_HOSTED_PREVIOUS")
        .env_remove("CARRICK_MOCK_ALL");
    if let Some(token) = token {
        command.env("CARRICK_TOKEN", token);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if args.contains(&"--json") {
        serde_json::from_slice(&output.stdout).unwrap()
    } else {
        Value::String(String::from_utf8(output.stdout).unwrap())
    }
}

fn init_repo(path: &Path) -> String {
    git(path, &["init", "-q"]);
    git(path, &["config", "user.email", "fixture@carrick.test"]);
    git(path, &["config", "user.name", "fixture"]);
    git(path, &["add", "."]);
    git(path, &["commit", "-qm", "fixture"]);
    git(path, &["rev-parse", "HEAD"])
}

fn blobs(root: &Path) -> Vec<Value> {
    std::fs::read_dir(root.join(".carrick/repos"))
        .unwrap()
        .map(|e| serde_json::from_slice(&std::fs::read(e.unwrap().path()).unwrap()).unwrap())
        .collect()
}

fn snapshot(root: &Path, local: &str, data: &[Value]) -> Value {
    let mut hash = Sha256::new();
    hash.update(b"https://api.carrick.tools\0");
    hash.update(TOKEN.as_bytes());
    let value = json!({
        "identity":format!("{:x}",hash.finalize()), "checked_at":"2026-09-10T10:00:00Z",
        "resolution": {
            "schema":"carrick.resolve-repos/0", "workspace":{"slug":"fixture","billing_tier":"free","installed":true},
            "allowance_sentence":"Candidates not refreshed since 2026-09-09.",
            "repos":[{"full_name":format!("example/{local}"),"connected":true,"project_id":"project-1","project_slug":"fixture",
                "services":data.iter().filter(|b| b["repo_name"] == local).map(|b| json!({"service":b["service_name"].as_str().unwrap_or(local),"hash":b["commit_hash"],"updated_at":"2026-09-09T10:00:00Z","scanner_version":"0.3.58"})).collect::<Vec<_>>() }],
            "project_repos":[{"project_slug":"fixture","repos":data.iter().map(|b| format!("example/{}", b["repo_name"].as_str().unwrap())).collect::<Vec<_>>() }]
        },
        "projects":{"project-1":data}
    });
    save_snapshot(root, &value);
    value
}
fn save_snapshot(root: &Path, value: &Value) {
    std::fs::create_dir_all(root.join(".carrick/hosted")).unwrap();
    std::fs::write(
        root.join(".carrick/hosted/snapshot.json"),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}
fn check(root: &Path, file: &str) -> Value {
    run(
        root,
        &["check", file, "--workspace", ".", "--json"],
        Some(TOKEN),
    )
}

#[test]
fn hosted_replay_tracks_working_tree_and_authentication_through_real_scan() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let repo = root.join("orders");
    std::fs::create_dir(&repo).unwrap();
    let source = "declare const app: { get(path: string, handler: () => unknown): void };\nfunction handler() { return { id: 1 }; }\napp.get(\"/hosted\", handler);\nfetch(\"https://example.test/local\", { method: \"GET\" });\n";
    std::fs::write(repo.join("app.ts"), source).unwrap();
    std::fs::write(
        repo.join("package.json"),
        r#"{"name":"orders","version":"1.0.0","dependencies":{"@remix-run/node":"^2.0.0"}}"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("carrick.json"),
        r#"{"service_name":"orders","include":["."]}"#,
    )
    .unwrap();
    std::fs::create_dir_all(repo.join("app/routes")).unwrap();
    std::fs::write(
        repo.join("app/routes/local.ts"),
        "export async function loader() { return { local: true }; }\n",
    )
    .unwrap();
    let commit = init_repo(&repo);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/orders.git",
        ],
    );
    std::fs::write(
        root.join("carrick-workspace.json"),
        r#"{"repos":["orders"]}"#,
    )
    .unwrap();
    run(root, &["index", "--workspace", "."], None);
    let baseline = check(root, "orders/app.ts");
    assert_eq!(baseline["hosted_state"], "not_signed_in");
    let mut hosted = blobs(root).remove(0);
    let start = source.find("app.get(").unwrap() + 1;
    let end = start + "app.get(\"/hosted\", handler)".len();
    hosted["file_results"] = json!({"app.ts":{"mounts":[],"data_calls":[],"endpoints":[{
        "candidate_id":format!("span:{start}-{end}"),"line_number":3,"owner_node":"app","method":"GET","path":"/hosted",
        "handler_name":"handler","pattern_matched":"fixture","call_expression_span_start":start,"call_expression_span_end":end,
        "resolution_source":"model"
    }]}});
    hosted["cached_detection"]["notes"] = json!("hosted detection marker");
    hosted["cached_guidance"]["http"]["triage_hints"] = json!("hosted guidance marker");
    hosted["cached_extraction_config"] = json!({"rules":[]});
    let original = snapshot(root, "orders", &[hosted]);
    run(root, &["index", "--workspace", "."], Some(TOKEN));
    let enriched = check(root, "orders/app.ts");
    assert_eq!(enriched["hosted_state"], "enriched", "{enriched:#}");
    assert!(
        enriched["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "/hosted" && r["source"] == "candidate"),
        "{enriched:#}"
    );
    let local = check(root, "orders/app/routes/local.ts");
    assert!(
        local["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "/local" && r["source"] == "fact"),
        "{local:#}"
    );
    assert_eq!(enriched["hosted"]["commit"], commit);
    assert_eq!(enriched["boundary"]["candidates_withheld_changed_files"], 0);
    assert!(
        enriched["boundary_note"]
            .as_str()
            .unwrap()
            .contains("could not refresh")
    );
    assert!(
        enriched["boundary_note"]
            .as_str()
            .unwrap()
            .ends_with("Candidates not refreshed since 2026-09-09.")
    );
    let wrong_account = run(
        root,
        &["check", "orders/app.ts", "--workspace", ".", "--json"],
        Some("another-workspace-token"),
    );
    assert_eq!(wrong_account["error"], "index_unreadable");
    let signed_out = run(root, &["status", "--workspace", ".", "--json"], None);
    assert_eq!(signed_out["error"], "index_unreadable");
    let replayed = blobs(root).remove(0);
    assert_eq!(
        replayed["cached_detection"]["notes"],
        "hosted detection marker"
    );
    assert_eq!(
        replayed["cached_guidance"]["http"]["triage_hints"],
        "hosted guidance marker"
    );
    assert_eq!(replayed["cached_extraction_config"], json!({"rules":[]}));
    // Manifest changes invalidate all three cached model configurations even
    // though the source file itself still matches the hosted commit.
    std::fs::write(
        repo.join("package.json"),
        r#"{"name":"orders","version":"2.0.0","dependencies":{"@remix-run/node":"^2.0.0"}}"#,
    )
    .unwrap();
    run(root, &["index", "--workspace", "."], Some(TOKEN));
    let changed_manifest = blobs(root).remove(0);
    assert_ne!(
        changed_manifest["cached_detection"]["notes"],
        "hosted detection marker"
    );
    assert_ne!(
        changed_manifest["cached_guidance"]["http"]["triage_hints"],
        "hosted guidance marker"
    );
    assert!(changed_manifest["cached_extraction_config"].is_null());
    git(&repo, &["checkout", "--", "package.json"]);
    // Dirty, staged, and committed changes all withhold the same hosted row.
    std::fs::write(repo.join("app.ts"), source.replace("/hosted", "/edited")).unwrap();
    for phase in 0..3 {
        if phase == 1 {
            git(&repo, &["add", "app.ts"]);
        }
        if phase == 2 {
            git(&repo, &["commit", "-qm", "edit"]);
        }
        run(root, &["index", "--workspace", "."], Some(TOKEN));
        let answer = check(root, "orders/app.ts");
        assert!(
            !answer["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["source"] == "candidate"),
            "phase {phase}: {answer:#}"
        );
        assert_eq!(answer["boundary"]["candidates_withheld_changed_files"], 1);
    }
    git(&repo, &["reset", "--hard", &commit]);
    std::fs::remove_file(repo.join("app.ts")).unwrap();
    std::fs::write(
        repo.join("new.ts"),
        "declare const app: any; app.get(\"/new\", () => 1);\n",
    )
    .unwrap();
    run(root, &["index", "--workspace", "."], Some(TOKEN));
    let deleted = check(root, "orders/app.ts");
    assert!(deleted["items"].as_array().unwrap().is_empty());
    assert_eq!(deleted["boundary"]["candidates_withheld_changed_files"], 1);
    assert!(
        !check(root, "orders/new.ts")["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["source"] == "candidate")
    );
    git(&repo, &["checkout", "--", "app.ts"]);
    std::fs::remove_file(repo.join("new.ts")).unwrap();
    for (field, value, state) in [
        (
            "commit_hash",
            json!("0000000000000000000000000000000000000000"),
            "commit_missing",
        ),
        ("cache_version", json!(0), "version_mismatch"),
    ] {
        let mut bad = original.clone();
        bad["projects"]["project-1"][0][field] = value;
        save_snapshot(root, &bad);
        run(root, &["index", "--workspace", "."], Some(TOKEN));
        let answer = check(root, "orders/app.ts");
        assert_eq!(answer["hosted_state"], state);
        assert!(
            !answer["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["source"] == "candidate")
        );
    }
    let mut day_one = original.clone();
    day_one["resolution"]["repos"][0]["services"] = json!([]);
    day_one["projects"]["project-1"] = json!([]);
    save_snapshot(root, &day_one);
    run(root, &["index", "--workspace", "."], Some(TOKEN));
    assert_eq!(check(root, "orders/app.ts")["hosted_state"], "no_index_yet");
    day_one["resolution"]["repos"][0] = json!({"full_name":"example/orders","connected":false});
    day_one["resolution"]["project_repos"] = json!([]);
    day_one["projects"] = json!({});
    save_snapshot(root, &day_one);
    run(root, &["index", "--workspace", "."], Some(TOKEN));
    assert_eq!(
        check(root, "orders/app.ts")["hosted_state"],
        "not_connected"
    );
    save_snapshot(root, &original);
    run(
        root,
        &["index", "--workspace", "."],
        Some("another-workspace-token"),
    );
    let switched = check(root, "orders/app.ts");
    assert!(switched["hosted"].is_null());
    assert!(
        !switched["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["source"] == "candidate")
    );
    // Read-only commands ignore even valid-looking credentials and never
    // open a connection to the configured proxy.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    for args in [
        vec!["check", "orders/app.ts", "--json"],
        vec!["touch", "orders/app.ts", "--json"],
        vec!["status", "--json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
            .args(args)
            .args(["--workspace", "."])
            .current_dir(root)
            .env("CARRICK_TOKEN", TOKEN)
            .env("XDG_CONFIG_HOME", root.join("empty-config"))
            .env("HTTPS_PROXY", &proxy)
            .env("https_proxy", &proxy)
            .env("ALL_PROXY", &proxy)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            listener.accept().is_err(),
            "read-only command attempted a network request"
        );
    }
    // Losing credentials cannot seed the prior signed-in model rows either.
    run(root, &["index", "--workspace", "."], None);
    assert_eq!(
        check(root, "orders/app.ts")["hosted_state"],
        "not_signed_in"
    );
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

#[test]
fn hosted_only_counterpart_keeps_real_type_verdict_and_has_no_local_navigation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local-mode-workspace");
    for name in ["catalog-web", "inventory-svc"] {
        let repo = root.join(name);
        copy_tree(&fixtures.join(name), &repo);
        std::fs::write(
            repo.join("carrick.json"),
            format!("{{\"name\":\"{name}\"}}"),
        )
        .unwrap();
        init_repo(&repo);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                &format!("https://github.com/example/{name}.git"),
            ],
        );
    }
    std::fs::write(
        root.join("carrick-workspace.json"),
        r#"{"repos":["catalog-web","inventory-svc"]}"#,
    )
    .unwrap();
    run(root, &["index", "--workspace", "."], None);
    let before = check(root, "inventory-svc/src/inventory.ts");
    let data = blobs(root);
    std::fs::remove_dir_all(root.join("catalog-web")).unwrap();
    std::fs::write(
        root.join("carrick-workspace.json"),
        r#"{"repos":["inventory-svc"]}"#,
    )
    .unwrap();
    snapshot(root, "inventory-svc", &data);
    run(root, &["index", "--workspace", "."], Some(TOKEN));
    let after = check(root, "inventory-svc/src/inventory.ts");
    assert!(!after["items"].as_array().unwrap().is_empty());
    assert_eq!(
        before["items"].as_array().unwrap().len(),
        after["items"].as_array().unwrap().len()
    );
    for (prior, current) in before["items"]
        .as_array()
        .unwrap()
        .iter()
        .zip(after["items"].as_array().unwrap())
    {
        assert_eq!(prior["verdict"]["state"], "resolved", "{prior:#}");
        assert_eq!(prior["verdict"]["result"], "compatible", "{prior:#}");
        assert_eq!(prior["verdict"], current["verdict"]);
        let counterparts = current["counterparts"].as_array().unwrap();
        assert!(!counterparts.is_empty(), "{current:#}");
        assert!(
            counterparts
                .iter()
                .all(|c| c["repo"].is_null() && c["remote"] == "example/catalog-web"),
            "{counterparts:#?}"
        );
    }
    // The exact native payload enters the npm decoder, both real hooks,
    // and the LSP server over stdio.
    use std::io::Write;
    let status = run(root, &["status", "--workspace", ".", "--json"], Some(TOKEN));
    let mut node = Command::new("node")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hosted-consumer.mjs"))
        .env(
            "CARRICK_CONSUMER_SOURCE",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("npm/carrick"),
        )
        .env("XDG_CONFIG_HOME", root.join("empty-config"))
        .env_remove("CARRICK_TOKEN")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    node.stdin
        .take()
        .unwrap()
        .write_all(
            &serde_json::to_vec(&json!({"check":after,"status":status,"root":root})).unwrap(),
        )
        .unwrap();
    let output = node.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "npm consumer seam: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let read_model: Value =
        serde_json::from_slice(&std::fs::read(root.join(".carrick/index.json")).unwrap()).unwrap();
    assert_eq!(read_model["repos"].as_array().unwrap().len(), 1);
    assert_eq!(read_model["repos"][0]["name"], "inventory-svc");
    // A local directory can share the hosted-only repo's basename while its
    // origin and service identify a different repository.
    std::fs::rename(root.join("inventory-svc"), root.join("catalog-web")).unwrap();
    std::fs::write(
        root.join("carrick-workspace.json"),
        r#"{"repos":["catalog-web"]}"#,
    )
    .unwrap();
    run(root, &["index", "--workspace", "."], Some(TOKEN));
    let collision = check(root, "catalog-web/src/inventory.ts");
    assert_eq!(collision["service"], "inventory-svc");
    assert_eq!(
        collision["items"][0]["counterparts"][0]["remote"],
        "example/catalog-web"
    );
    assert!(collision["items"][0]["counterparts"][0]["repo"].is_null());
    let model: Value =
        serde_json::from_slice(&std::fs::read(root.join(".carrick/index.json")).unwrap()).unwrap();
    assert_eq!(model["repos"][0]["services"].as_array().unwrap().len(), 1);
    assert_eq!(model["repos"][0]["services"][0]["name"], "inventory-svc");
    assert!(
        model["repos"][0]["files"]
            .get("app/routes/api.v1.widgets.$widgetId.ts")
            .is_none()
    );
    assert!(
        blobs(root)
            .iter()
            .all(|b| b["service_name"] == "inventory-svc")
    );
}
