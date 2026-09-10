use carrick::service_derivation::resolve;
use std::path::Path;

fn write(root: &Path, file: &str, body: &str) {
    let target = root.join(file);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(target, body).unwrap();
}

#[test]
fn npm_services_use_member_names_and_nearest_tsconfig_and_roundtrip_identically() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{"private":true,"workspaces":["packages/*"]}"#,
    );
    write(root, "tsconfig.json", "{}");
    write(
        root,
        "packages/api/package.json",
        r#"{"name":"@sample/api","dependencies":{"@sample/shared":"workspace:*"}}"#,
    );
    write(root, "packages/api/tsconfig.json", "{}");
    write(
        root,
        "packages/shared/package.json",
        r#"{"name":"@sample/shared","exports":"./src/index.ts"}"#,
    );
    write(
        root,
        "packages/shared/src/index.ts",
        "export interface Item { id: string }",
    );
    let derived = resolve(root).unwrap();
    assert_eq!(derived.reason, "npm workspaces");
    assert_eq!(derived.services.len(), 2);
    assert_eq!(
        derived.services[0].service_name.as_deref(),
        Some("@sample/api")
    );
    assert_eq!(
        derived.services[0].tsconfig.as_deref(),
        Some("tsconfig.json")
    );
    assert_eq!(
        derived.services[1].tsconfig.as_deref(),
        Some("../../tsconfig.json")
    );
    assert!(derived.services[0].include.is_empty());
    write(
        root,
        "carrick.json",
        &serde_json::to_string(&derived.config).unwrap(),
    );
    let explicit = resolve(root).unwrap();
    assert_eq!(explicit.reason, "carrick.json");
    assert_eq!(
        serde_json::to_value(derived.services).unwrap(),
        serde_json::to_value(explicit.services).unwrap()
    );
}

#[test]
fn pnpm_yaml_exclusions_and_npm_overlap_are_deduplicated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{"workspaces":{"packages":["apps/*"]}}"#,
    );
    write(
        root,
        "pnpm-workspace.yaml",
        "packages:\n  - 'apps/*'\n  - 'libs/*'\n  - '!**/excluded'\n",
    );
    for member in ["apps/api", "libs/shared", "libs/excluded"] {
        write(root, &format!("{member}/package.json"), "{}");
    }
    let services = resolve(root).unwrap().services;
    assert_eq!(
        services
            .iter()
            .map(|s| s.directory.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["apps/api", "libs/shared"]
    );
}

#[test]
fn explicit_config_preserves_shared_sources_and_refuses_invalid_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        "malformed but irrelevant to explicit service selection",
    );
    write(root, "api/index.ts", "");
    write(root, "shared/index.ts", "");
    let text = r#"{"services":[{"name":"chosen","directory":"api","include":["shared"],"externalDomains":["example.com"]}]}"#;
    write(root, "carrick.json", text);
    let service = resolve(root).unwrap().services.remove(0);
    assert_eq!(service.include, ["shared"]);
    assert!(service.external_domains.contains("example.com"));
    assert_eq!(
        std::fs::read_to_string(root.join("carrick.json")).unwrap(),
        text
    );
    write(root, "carrick.json", "{broken");
    assert!(resolve(root).is_err());
    write(
        root,
        "carrick.json",
        r#"{"services":[{"directory":"gone"}]}"#,
    );
    assert!(resolve(root).is_err());
}

#[test]
fn plain_repo_and_own_repo_layout() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "package.json", r#"{"name":"one"}"#);
    let plain = resolve(dir.path()).unwrap();
    assert_eq!(plain.services.len(), 1);
    assert!(plain.services[0].directory.is_none());
    assert!(plain.services[0].service_name.is_none());
    let own = resolve(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    assert_eq!(own.reason, "carrick.json");
    assert_eq!(own.services[0].directory.as_deref(), Some("src/sidecar"));
}

#[test]
fn native_init_preview_and_ci_select_identical_services() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "package.json", r#"{"workspaces":["apps/*"]}"#);
    write(dir.path(), "apps/api/package.json", r#"{"name":"api"}"#);
    let expected = resolve(dir.path()).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_carrick"))
        .args(["derive", "--workspace"])
        .arg(dir.path())
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual["schema"], "carrick.derive/0");
    assert_eq!(
        actual["repos"][0]["services"],
        serde_json::to_value(expected.services).unwrap()
    );
    assert_eq!(actual["repos"][0]["config"], expected.config);
    assert!(!dir.path().join("carrick.json").exists());
    assert!(!dir.path().join(".carrick").exists());
}

#[test]
fn native_preview_refuses_a_broken_explicit_config_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "carrick.json", "{broken");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_carrick"))
        .args(["derive", "--workspace"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("carrick.json")).unwrap(),
        "{broken"
    );
    assert!(!dir.path().join(".carrick").exists());
}

#[test]
fn malformed_workspace_and_unmatched_members_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    for text in [
        r#"{"workspaces":42}"#,
        r#"{"workspaces":["missing/*"]}"#,
        r#"{"workspaces":["../outside/*"]}"#,
        r#"{"workspaces":["apps/{one,two}"]}"#,
    ] {
        write(dir.path(), "package.json", text);
        assert!(resolve(dir.path()).is_err(), "{text}");
    }
    write(dir.path(), "package.json", "{}");
    write(dir.path(), "pnpm-workspace.yaml", "packages: 42");
    assert!(resolve(dir.path()).is_err());
}

#[test]
fn deno_adapter_supplies_members_and_leaves_config_discovery_to_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "deno.jsonc",
        "{ // root\n\"workspace\": [\"./api\", \"./shared\"] }",
    );
    write(dir.path(), "api/deno.json", r#"{"name":"@sample/api"}"#);
    write(
        dir.path(),
        "shared/deno.json",
        r#"{"name":"@sample/shared","exports":"./mod.ts"}"#,
    );
    write(
        dir.path(),
        "tsconfig.json",
        r#"{"compilerOptions":{"types":["node"]}}"#,
    );
    let derived = resolve(dir.path()).unwrap();
    assert!(derived.services.iter().all(|s| s.tsconfig.is_none()));
    assert_eq!(derived.services.len(), 2);
    assert_eq!(
        derived.services[0].service_name.as_deref(),
        Some("@sample/api")
    );
    assert!(
        derived
            .warnings
            .iter()
            .any(|w| w.contains("Deno") && w.contains("type"))
    );
}

#[test]
fn deno_missing_runtime_fails_before_cloud_or_type_startup() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "deno.json", "{}");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(dir.path())
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("Deno is required"), "{text}");
    assert!(!text.contains("can't scan Deno-native"), "{text}");
}

#[test]
fn explicit_typescript_config_keeps_node_path_in_deno_workspace() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "deno.json", "{}");
    write(dir.path(), "tsconfig.json", "{}");
    write(
        dir.path(),
        "carrick.json",
        r#"{"tsconfig":"tsconfig.json"}"#,
    );
    let derived = resolve(dir.path()).unwrap();
    assert!(carrick::deno_support::service_manifest(dir.path(), &derived.services[0]).is_none());
}

#[test]
fn unrelated_node_subtree_does_not_inherit_deno_but_declared_member_does() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "deno.json", "{}");
    write(dir.path(), "tools/package.json", r#"{"name":"tooling"}"#);
    let service = carrick::config::Config {
        directory: Some("tools".into()),
        ..Default::default()
    };
    assert!(carrick::deno_support::service_manifest(dir.path(), &service).is_none());
    write(dir.path(), "deno.json", r#"{"workspace":["./tools"]}"#);
    assert!(carrick::deno_support::service_manifest(dir.path(), &service).is_some());
    write(dir.path(), "deno.json", "{}");
    write(dir.path(), "tools/deno.json", "{}");
    assert!(carrick::deno_support::service_manifest(dir.path(), &service).is_some());
}

#[cfg(unix)]
#[test]
fn unsupported_deno_version_fails_before_analysis() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "deno.json", "{}");
    write(
        dir.path(),
        "bin/deno",
        "#!/bin/sh\necho 'deno 2.6.0 (stable)'\n",
    );
    std::fs::set_permissions(
        dir.path().join("bin/deno"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(dir.path())
        .env("PATH", dir.path().join("bin"))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success());
    assert!(text.contains("2.9.4 or newer"), "{text}");
}
