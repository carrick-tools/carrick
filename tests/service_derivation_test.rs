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
    // The service entries the command prints are the configuration and the
    // member facts in one object (carrick#994); `config` stays the skeleton.
    assert_eq!(
        actual["repos"][0]["services"],
        serde_json::to_value(expected.service_documents()).unwrap()
    );
    assert_eq!(actual["repos"][0]["services"][0]["private"], false);
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

#[test]
fn alternate_deno_config_is_rejected_by_the_shared_service_plan() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "deno.json", "{}");
    write(
        dir.path(),
        "deno.jsonc",
        r#"{"compilerOptions":{"strict":false}}"#,
    );
    write(dir.path(), "carrick.json", r#"{"tsconfig":"deno.jsonc"}"#);
    let error = resolve(dir.path()).unwrap_err();
    assert!(error.contains("nearest Deno manifest"), "{error}");
    write(dir.path(), "carrick.json", r#"{"tsconfig":"deno.json"}"#);
    assert!(resolve(dir.path()).is_ok());
    write(dir.path(), "tsconfig.json", "{}");
    write(
        dir.path(),
        "carrick.json",
        r#"{"tsconfig":"tsconfig.json"}"#,
    );
    assert!(resolve(dir.path()).is_ok());
}

#[test]
fn generated_deno_cache_does_not_change_discovered_sources() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "deno.json", "{}");
    write(dir.path(), "src/api.ts", "export const value = 1;");
    let discover = || {
        carrick::file_finder::find_files(
            dir.path().to_str().unwrap(),
            &carrick::packages::MANIFEST_SKIP_DIRS,
        )
        .0
    };
    let before = discover();
    assert_eq!(before.len(), 1);
    write(
        dir.path(),
        ".carrick/deno/entry.ts",
        "import '../../src/api.ts';",
    );
    write(
        dir.path(),
        ".carrick/deno/runtime.d.ts",
        "declare namespace Deno { const version: string; }",
    );
    write(
        dir.path(),
        ".carrick/deno/remote/mod.ts",
        "export const cached = 2;",
    );
    assert_eq!(
        discover(),
        before,
        "type preparation must not feed generated sources back into the scanner"
    );
}

/// carrick#994. Every workspace member is derived as a service, and the whole
/// application-versus-library decision then lived in the scaffold
/// instructions, so the agent had to re-derive it by walking imports. These
/// are the facts that decision is made on, carried per member.
#[test]
fn member_facts_carry_what_decides_application_from_library() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{"private":true,"workspaces":["apps/*","packages/*"]}"#,
    );
    write(root, "tsconfig.json", "{}");
    write(
        root,
        "apps/gateway/package.json",
        r#"{"name":"@sample/gateway","private":true,"bin":{"gateway":"./bin.js"},"dependencies":{"@sample/shared":"workspace:*"}}"#,
    );
    write(root, "apps/gateway/Dockerfile", "FROM node:24\n");
    write(
        root,
        "packages/shared/package.json",
        r#"{"name":"@sample/shared","main":"./src/index.js","exports":"./src/index.ts"}"#,
    );

    let derived = resolve(root).unwrap();
    assert_eq!(derived.services.len(), 2);
    assert_eq!(derived.members.len(), derived.services.len());
    assert_eq!(
        derived.services[0].service_name.as_deref(),
        Some("@sample/gateway")
    );

    // The application: private, executable, deployed, imported by nobody.
    let gateway = &derived.members[0];
    assert!(gateway.private);
    assert!(gateway.bin);
    assert!(!gateway.main);
    assert!(!gateway.exports);
    assert_eq!(gateway.deploy_config, vec!["Dockerfile".to_string()]);
    assert!(gateway.workspace_dependents.is_empty());

    // The library: publishable, an import surface, nothing deployed beside it,
    // and named by the member that depends on it.
    let shared = &derived.members[1];
    assert!(!shared.private);
    assert!(!shared.bin);
    assert!(shared.main);
    assert!(shared.exports);
    assert!(shared.deploy_config.is_empty());
    assert_eq!(
        shared.workspace_dependents,
        vec!["@sample/gateway".to_string()]
    );

    // The facts ride on the service entries of `carrick.derive/0`, beside the
    // configuration, and never in the `carrick.json` skeleton: `private` is
    // something a repository states about itself, not a key Carrick accepts.
    let documents = derived.service_documents();
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[1]["serviceName"], "@sample/shared");
    assert_eq!(documents[1]["exports"], true);
    assert_eq!(documents[1]["private"], false);
    assert_eq!(documents[1]["workspace_dependents"][0], "@sample/gateway");
    assert_eq!(documents[0]["deploy_config"][0], "Dockerfile");
    let written = serde_json::to_string(&derived.config).unwrap();
    assert!(!written.contains("workspace_dependents"), "{written}");
    assert!(!written.contains("deploy_config"), "{written}");

    // And an explicit config is described the same way: the facts are read
    // from the repository, not from how the services were chosen.
    write(root, "carrick.json", &written);
    let explicit = resolve(root).unwrap();
    assert_eq!(explicit.reason, "carrick.json");
    assert_eq!(explicit.members, derived.members);
}

/// A Deno member names a sibling by path far more often than by published
/// identity, and the dependency map holds registry identities only. Without
/// the import-map targets every Deno member would report no dependents, which
/// is a claim rather than an absence.
#[test]
fn a_deno_member_imported_by_path_names_its_dependent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "deno.json",
        r#"{"workspace":["./apps/api","./packages/lib"]}"#,
    );
    write(
        root,
        "apps/api/deno.json",
        r#"{"name":"@sample/api","imports":{"@sample/lib":"../../packages/lib/mod.ts","zod":"npm:zod@^3"}}"#,
    );
    write(root, "apps/api/mod.ts", "export const a = 1;");
    write(
        root,
        "packages/lib/deno.json",
        r#"{"name":"@sample/lib","exports":"./mod.ts"}"#,
    );
    write(root, "packages/lib/mod.ts", "export const b = 2;");

    let derived = resolve(root).unwrap();
    assert_eq!(derived.services.len(), 2);
    assert_eq!(
        derived.services[0].service_name.as_deref(),
        Some("@sample/api")
    );
    assert!(derived.members[0].workspace_dependents.is_empty());
    assert_eq!(
        derived.members[1].workspace_dependents,
        vec!["@sample/api".to_string()]
    );
    assert!(derived.members[1].exports);
    // A registry dependency is not a member, whatever it is named.
    assert_eq!(derived.members[1].workspace_dependents.len(), 1);
}
