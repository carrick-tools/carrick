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
    // Standing advice about a Deno proposal, not something this run found: it
    // rides in the proposal document and the terminal never prints it
    // (carrick#1032).
    assert!(
        derived
            .notes
            .iter()
            .any(|w| w.contains("Deno") && w.contains("type"))
    );
    assert!(derived.warnings.is_empty(), "{:?}", derived.warnings);
}

#[test]
fn deno_missing_runtime_fails_before_cloud_or_type_startup() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "deno.json", "{}");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(dir.path())
        .env("PATH", "")
        // A laptop run that fails before start-scan reports it to the cloud
        // (carrick#1096); the mock keeps a test's failure off the wire.
        .env("CARRICK_MOCK_ALL", "1")
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
        .env("CARRICK_MOCK_ALL", "1")
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

/// The ordinary Deno workspace: members import each other by the NAME the
/// workspace makes importable, and nothing records that edge — not a
/// dependency entry, not an import map, not the lock file. Every member
/// carried `exports: true` and an empty dependents list, so neither field
/// separated the apps from the library they share (carrick#1007 item 6).
#[test]
fn a_deno_member_imported_by_workspace_name_names_its_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "deno.json",
        r#"{"workspace":["./apps/gateway","./apps/ledger","./packages/shared"]}"#,
    );
    write(
        root,
        "apps/gateway/deno.json",
        r#"{"name":"@sample/gateway","exports":"./main.ts"}"#,
    );
    // Types only, which is still an import: a shared types package is exactly
    // the member a dependents list exists to name.
    write(
        root,
        "apps/gateway/main.ts",
        "import type { Entry } from \"@sample/shared\";\nexport const port = 8080;\n",
    );
    write(
        root,
        "apps/ledger/deno.json",
        r#"{"name":"@sample/ledger","exports":"./main.ts"}"#,
    );
    write(
        root,
        "apps/ledger/main.ts",
        "import { format } from \"@sample/shared/money\";\nexport const port = 8081;\n",
    );
    write(
        root,
        "packages/shared/deno.json",
        r#"{"name":"@sample/shared","exports":"./mod.ts"}"#,
    );
    write(
        root,
        "packages/shared/mod.ts",
        "export type Entry = { id: string };\nexport const format = (n: number) => `${n}`;\n",
    );

    let derived = resolve(root).unwrap();
    let by_name = |name: &str| {
        derived
            .services
            .iter()
            .position(|service| service.service_name.as_deref() == Some(name))
            .map(|index| &derived.members[index])
            .unwrap_or_else(|| panic!("no member named {name}"))
    };

    // Both apps declare `exports`, as every Deno member must to be importable
    // at all, so that field cannot be what separates them.
    assert!(by_name("@sample/gateway").exports);
    assert!(by_name("@sample/ledger").exports);
    assert!(by_name("@sample/shared").exports);

    assert_eq!(
        by_name("@sample/shared").workspace_dependents,
        vec!["@sample/gateway".to_string(), "@sample/ledger".to_string()]
    );
    // An application is imported by nobody, which is the whole distinction.
    assert!(by_name("@sample/gateway").workspace_dependents.is_empty());
    assert!(by_name("@sample/ledger").workspace_dependents.is_empty());
}

/// An npm workspace answers from its manifests and reads no source: a member
/// that imports a sibling it does not declare is a manifest defect, and
/// inventing the edge here would hide it.
#[test]
fn an_npm_member_still_answers_from_its_manifest_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
    );
    write(
        root,
        "packages/app/package.json",
        r#"{"name":"@sample/app","private":true}"#,
    );
    write(
        root,
        "packages/app/index.ts",
        "import { thing } from \"@sample/lib\";\nexport const app = thing;\n",
    );
    write(
        root,
        "packages/lib/package.json",
        r#"{"name":"@sample/lib","main":"index.js"}"#,
    );
    write(root, "packages/lib/index.ts", "export const thing = 1;\n");

    let derived = resolve(root).unwrap();
    let lib = derived
        .services
        .iter()
        .position(|service| service.service_name.as_deref() == Some("@sample/lib"))
        .expect("the library is a member");
    assert!(derived.members[lib].workspace_dependents.is_empty());
}

/// carrick#553. A derived member inside another member is that member's own
/// source: the outer member's walk leaves it out, as it does for a declared
/// list, and the proposal written out as `carrick.json` resolves the same way.
#[test]
fn a_workspace_member_inside_another_member_is_left_out_of_the_outer_walk() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{"private":true,"workspaces":["packages/*","packages/*/plugins/*"]}"#,
    );
    write(
        root,
        "packages/api/package.json",
        r#"{"name":"@sample/api"}"#,
    );
    write(root, "packages/api/index.ts", "export const api = 1;\n");
    write(
        root,
        "packages/api/plugins/audit/package.json",
        r#"{"name":"@sample/audit"}"#,
    );
    write(
        root,
        "packages/api/plugins/audit/index.ts",
        "export const audit = 1;\n",
    );

    let read_by = |services: &[carrick::config::Config], name: &str| -> Vec<String> {
        let service = services
            .iter()
            .find(|service| service.service_name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no service named {name}"));
        let scanned = root.canonicalize().unwrap();
        let (files, _) = carrick::file_finder::find_service_files(
            scanned.to_str().unwrap(),
            service,
            &carrick::packages::MANIFEST_SKIP_DIRS,
        );
        files
            .iter()
            .map(|file| {
                file.strip_prefix(&scanned)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    };

    let derived = resolve(root).unwrap();
    assert_eq!(
        read_by(&derived.services, "@sample/api"),
        ["packages/api/index.ts"]
    );
    assert_eq!(
        read_by(&derived.services, "@sample/audit"),
        ["packages/api/plugins/audit/index.ts"]
    );

    write(
        root,
        "carrick.json",
        &serde_json::to_string(&derived.config).unwrap(),
    );
    let explicit = resolve(root).unwrap();
    assert_eq!(explicit.reason, "carrick.json");
    assert_eq!(
        read_by(&explicit.services, "@sample/api"),
        ["packages/api/index.ts"]
    );
    assert_eq!(
        read_by(&explicit.services, "@sample/audit"),
        ["packages/api/plugins/audit/index.ts"]
    );
}

/// An application that installs on its own: a manifest, the lockfile its
/// package manager wrote beside it, and source.
fn lockfile_rooted_app(root: &Path, directory: &str, name: &str, lockfile: &str) {
    write(
        root,
        &format!("{directory}/package.json"),
        &format!(r#"{{"name":"{name}","private":true,"dependencies":{{"left-pad":"1.3.0"}}}}"#),
    );
    write(root, &format!("{directory}/{lockfile}"), "");
    write(root, &format!("{directory}/tsconfig.json"), "{}");
    write(
        root,
        &format!("{directory}/src/main.ts"),
        "export const port = 3000;\n",
    );
}

/// A root manifest that installs tooling for the repository and declares no
/// workspace, with a lockfile of its own.
fn tooling_root(root: &Path) {
    write(
        root,
        "package.json",
        r#"{"private":true,"scripts":{"dev":"run-p dev:*"},"devDependencies":{"npm-run-all":"^4.1.5"}}"#,
    );
    write(root, "package-lock.json", "{}");
}

fn directories(derived: &carrick::service_derivation::ServiceDerivation) -> Vec<&str> {
    derived
        .services
        .iter()
        .map(|service| service.directory.as_deref().unwrap_or("."))
        .collect()
}

/// Everything a derivation states, for a test that says nothing moved.
fn stated(derived: &carrick::service_derivation::ServiceDerivation) -> serde_json::Value {
    serde_json::json!({
        "reason": derived.reason,
        "services": derived.service_documents(),
        "config": derived.config,
        "warnings": derived.warnings,
        "notes": derived.notes,
    })
}

/// The files a scan of `root` reads through `walk`, relative to the
/// repository and sorted.
fn relative_files(root: &Path, files: Vec<std::path::PathBuf>) -> Vec<String> {
    let mut relative: Vec<String> = files
        .iter()
        .map(|file| {
            file.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    relative.sort();
    relative
}

/// What the service proposed at `directory` reads, with the same walk a scan
/// uses.
fn read_by(
    root: &Path,
    derived: &carrick::service_derivation::ServiceDerivation,
    directory: &str,
) -> Vec<String> {
    let root = root.canonicalize().unwrap();
    let service = derived
        .services
        .iter()
        .find(|service| service.directory.as_deref().unwrap_or(".") == directory)
        .unwrap_or_else(|| panic!("no service proposed at {directory}"));
    let (files, _) = carrick::file_finder::find_service_files(
        root.to_str().unwrap(),
        service,
        &carrick::packages::MANIFEST_SKIP_DIRS,
    );
    relative_files(&root, files)
}

/// What a proposal has to keep: every file the one service read is read by
/// exactly one proposed service.
fn assert_every_file_is_read_once(
    root: &Path,
    derived: &carrick::service_derivation::ServiceDerivation,
) {
    let canonical = root.canonicalize().unwrap();
    let (whole, _) = carrick::file_finder::find_files(
        canonical.to_str().unwrap(),
        &carrick::packages::MANIFEST_SKIP_DIRS,
    );
    let mut read: Vec<String> = directories(derived)
        .into_iter()
        .flat_map(|directory| read_by(root, derived, directory))
        .collect();
    read.sort();
    assert_eq!(read, relative_files(&canonical, whole));
}

/// carrick#1854. A repository whose applications each carry their own
/// manifest and lockfile, under a root that declares no workspace, is as many
/// installs as it has lockfiles. It was proposed as one service rooted at the
/// repository, which folds a client and the server it calls into one.
#[test]
fn lockfile_rooted_apps_with_no_declared_workspace_are_each_a_service() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    tooling_root(root);
    lockfile_rooted_app(root, "web", "@sample/web", "package-lock.json");
    lockfile_rooted_app(root, "api", "@sample/api", "pnpm-lock.yaml");

    let derived = resolve(root).unwrap();
    assert_eq!(derived.reason, "lockfile-rooted packages");
    assert_eq!(directories(&derived), ["api", "web"]);
    assert_eq!(
        derived
            .services
            .iter()
            .map(|service| service.service_name.as_deref())
            .collect::<Vec<_>>(),
        [Some("@sample/api"), Some("@sample/web")]
    );
    assert!(
        derived
            .services
            .iter()
            .all(|service| service.tsconfig.as_deref() == Some("tsconfig.json"))
    );
    assert_eq!(derived.members.len(), 2);
    // The root holds no source of its own, so it is not a service: a scan
    // refuses one with nothing in it.
    assert_every_file_is_read_once(root, &derived);
    assert!(derived.warnings.is_empty(), "{:?}", derived.warnings);
    assert!(
        derived
            .notes
            .iter()
            .any(|note| note.contains("lockfile") && note.contains("carrick.json")),
        "{:?}",
        derived.notes
    );

    // The proposal is the configuration: written out, it selects the same
    // services.
    write(
        root,
        "carrick.json",
        &serde_json::to_string(&derived.config).unwrap(),
    );
    let explicit = resolve(root).unwrap();
    assert_eq!(explicit.reason, "carrick.json");
    assert_eq!(
        serde_json::to_value(&derived.services).unwrap(),
        serde_json::to_value(&explicit.services).unwrap()
    );
}

/// The rule reads a lockfile beside a manifest, whichever package manager
/// wrote it, at whatever depth the application sits.
#[test]
fn every_recognised_lockfile_roots_a_service_at_any_depth() {
    for lockfile in [
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tooling_root(root);
        lockfile_rooted_app(root, "apps/storefront", "storefront", lockfile);
        lockfile_rooted_app(root, "services/billing/http", "billing", lockfile);
        let derived = resolve(root).unwrap();
        assert_eq!(
            directories(&derived),
            ["apps/storefront", "services/billing/http"],
            "{lockfile}"
        );
        assert_eq!(derived.reason, "lockfile-rooted packages", "{lockfile}");
    }

    // A Deno application states the same thing with its own two files, and is
    // typed from its manifest rather than a tsconfig.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    tooling_root(root);
    lockfile_rooted_app(root, "web", "web", "yarn.lock");
    write(root, "edge/deno.json", r#"{"name":"@sample/edge"}"#);
    write(root, "edge/deno.lock", "{}");
    write(root, "edge/main.ts", "export const port = 8000;\n");
    let derived = resolve(root).unwrap();
    assert_eq!(directories(&derived), ["edge", "web"]);
    assert_eq!(
        derived.services[0].service_name.as_deref(),
        Some("@sample/edge")
    );
    assert!(derived.services[0].tsconfig.is_none());
    assert!(
        derived.notes.iter().any(|note| note.contains("Deno")),
        "{:?}",
        derived.notes
    );
}

/// What the rule does not read as an install: a manifest with no lockfile
/// beside it, a lockfile-rooted directory holding no source a scan reads, and
/// one a scan of the repository never enters. None of them is a service, and
/// none of their source is lost: it is the root's, which is proposed beside
/// the package for exactly that. A manifest that declares dependencies with
/// no lockfile is said, because an uncommitted lockfile is how a package
/// usually ends up there.
#[test]
fn a_manifest_alone_or_a_directory_without_scanned_source_is_not_a_service() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    tooling_root(root);
    lockfile_rooted_app(root, "web", "web", "package-lock.json");
    // A manifest that only marks a subtree: it declares nothing to install.
    write(root, "worker/package.json", r#"{"name":"worker"}"#);
    write(root, "worker/index.ts", "export const queue = 'jobs';\n");
    // A package that declares dependencies and carries no lockfile.
    write(
        root,
        "jobs/package.json",
        r#"{"name":"jobs","dependencies":{"left-pad":"1.3.0"}}"#,
    );
    write(root, "jobs/run.ts", "export const run = 1;\n");
    // The same inside the lockfile-rooted package: that lockfile may be the
    // one that installs it, so it is not reported.
    write(
        root,
        "web/plugins/theme/package.json",
        r#"{"name":"theme","dependencies":{"left-pad":"1.3.0"}}"#,
    );
    write(
        root,
        "web/plugins/theme/index.ts",
        "export const theme = 1;\n",
    );
    // Source with no manifest of its own.
    write(root, "shared/format.ts", "export const format = String;\n");
    // An install with nothing a scan reads in it.
    write(root, "infra/package.json", r#"{"name":"infra"}"#);
    write(root, "infra/package-lock.json", "{}");
    write(root, "infra/stack.json", "{}");
    // An end-to-end suite: a scan of this repository never reads it.
    write(root, "e2e/package.json", r#"{"name":"suite"}"#);
    write(root, "e2e/package-lock.json", "{}");
    write(root, "e2e/login.ts", "export const login = 1;\n");
    // An installed dependency ships both files too.
    write(
        root,
        "web/node_modules/dep/package.json",
        r#"{"name":"dep"}"#,
    );
    write(root, "web/node_modules/dep/package-lock.json", "{}");
    write(
        root,
        "web/node_modules/dep/index.js",
        "module.exports = 1;\n",
    );

    let derived = resolve(root).unwrap();
    assert_eq!(directories(&derived), [".", "web"]);
    assert_eq!(
        read_by(root, &derived, "."),
        ["jobs/run.ts", "shared/format.ts", "worker/index.ts"]
    );
    assert_every_file_is_read_once(root, &derived);
    assert_eq!(
        derived.warnings,
        [
            "`jobs`: a manifest that declares dependencies with no lockfile beside it, so not proposed as a service and indexed with the repository root. An uncommitted lockfile is the usual cause: commit it, or declare the service in carrick.json."
        ]
    );
}

/// Two root manifests: one that installs tooling and one that is an
/// application. The rule reads neither for what it declares, only the tree
/// for where the source sits.
const ROOT_MANIFESTS: [&str; 2] = [
    r#"{"private":true,"devDependencies":{"npm-run-all":"^4.1.5"}}"#,
    r#"{"name":"shop","main":"./src/index.js","dependencies":{"express":"^4.19.2"}}"#,
];

/// A root that holds source of its own is proposed beside the lockfile-rooted
/// packages under it, whatever its manifest declares, and stays the unnamed
/// service the repository already was. It reads its own source and none of
/// theirs, so every file the one service read is still read, once.
#[test]
fn a_root_with_source_of_its_own_is_proposed_beside_its_packages() {
    for manifest in ROOT_MANIFESTS {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "package.json", manifest);
        write(root, "package-lock.json", "{}");
        write(root, "tsconfig.json", "{}");
        write(root, "src/index.ts", "export const shop = 1;\n");
        let before = resolve(root).unwrap();
        assert_eq!(before.reason, "single repository", "{manifest}");
        assert_eq!(directories(&before), ["."], "{manifest}");

        lockfile_rooted_app(root, "functions", "functions", "package-lock.json");
        lockfile_rooted_app(root, "website", "website", "yarn.lock");
        let derived = resolve(root).unwrap();
        assert_eq!(derived.reason, "lockfile-rooted packages", "{manifest}");
        assert_eq!(
            directories(&derived),
            [".", "functions", "website"],
            "{manifest}"
        );
        // The root is the service it was: unnamed, its tsconfig beside it.
        assert_eq!(
            serde_json::to_value(&derived.services[0]).unwrap(),
            serde_json::to_value(&before.services[0]).unwrap(),
            "{manifest}"
        );
        assert_eq!(read_by(root, &derived, "."), ["src/index.ts"], "{manifest}");
        assert_eq!(
            read_by(root, &derived, "functions"),
            ["functions/src/main.ts"],
            "{manifest}"
        );
        assert_every_file_is_read_once(root, &derived);
        assert!(derived.warnings.is_empty(), "{manifest}");

        // Written out as `carrick.json`, the proposal reads the same files.
        write(
            root,
            "carrick.json",
            &serde_json::to_string(&derived.config).unwrap(),
        );
        let explicit = resolve(root).unwrap();
        assert_eq!(explicit.reason, "carrick.json", "{manifest}");
        assert_eq!(
            read_by(root, &explicit, "."),
            ["src/index.ts"],
            "{manifest}"
        );
        assert_every_file_is_read_once(root, &explicit);
    }
}

/// A root holding no source of its own is not a service, whatever its
/// manifest declares: a scan refuses a service with nothing in it. A root
/// with no manifest at all is read the same way as one with a manifest: it
/// is the service for its loose source.
#[test]
fn a_root_is_a_service_only_for_source_of_its_own() {
    for manifest in ROOT_MANIFESTS {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "package.json", manifest);
        write(root, "package-lock.json", "{}");
        lockfile_rooted_app(root, "web", "web", "package-lock.json");
        lockfile_rooted_app(root, "api", "api", "package-lock.json");
        let derived = resolve(root).unwrap();
        assert_eq!(directories(&derived), ["api", "web"], "{manifest}");
        assert_every_file_is_read_once(root, &derived);
        assert!(derived.warnings.is_empty(), "{manifest}");
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    lockfile_rooted_app(root, "web", "web", "package-lock.json");
    write(root, "scripts/release.ts", "export const release = 1;\n");
    write(root, "seed.js", "module.exports = 1;\n");
    let derived = resolve(root).unwrap();
    assert_eq!(directories(&derived), [".", "web"]);
    assert_eq!(
        read_by(root, &derived, "."),
        ["scripts/release.ts", "seed.js"]
    );
    assert_every_file_is_read_once(root, &derived);
    assert!(derived.warnings.is_empty(), "{:?}", derived.warnings);
}

/// A single-package repository is one service whatever sits beside its
/// manifest: its own lockfile, or a nested manifest that states no install.
#[test]
fn a_single_package_repository_with_a_lockfile_does_not_move() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", r#"{"name":"one","type":"module"}"#);
    write(root, "src/index.ts", "export const one = 1;\n");
    let before = stated(&resolve(root).unwrap());

    write(root, "yarn.lock", "");
    // A marker manifest that only sets the module type of a subtree.
    write(root, "src/legacy/package.json", r#"{"type":"commonjs"}"#);
    write(root, "src/legacy/index.js", "module.exports = 1;\n");
    let derived = resolve(root).unwrap();
    assert_eq!(derived.reason, "single repository");
    assert_eq!(stated(&derived), before);
}

/// A repository that declares a workspace is described by its declaration
/// alone. A lockfile inside a member, and a lockfile-rooted package the
/// patterns do not claim, change nothing about which services are proposed.
#[test]
fn a_declared_workspace_is_not_moved_by_lockfiles_inside_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{"private":true,"workspaces":["packages/*"]}"#,
    );
    write(root, "package-lock.json", "{}");
    write(
        root,
        "packages/api/package.json",
        r#"{"name":"@sample/api"}"#,
    );
    write(root, "packages/api/index.ts", "export const api = 1;\n");
    write(
        root,
        "packages/web/package.json",
        r#"{"name":"@sample/web"}"#,
    );
    write(root, "packages/web/index.ts", "export const web = 1;\n");
    let before = stated(&resolve(root).unwrap());

    // A member that also carries a lockfile of its own.
    write(root, "packages/api/package-lock.json", "{}");
    // A lockfile-rooted package inside a member, and one outside every
    // pattern.
    lockfile_rooted_app(root, "packages/web/demo", "demo", "package-lock.json");
    lockfile_rooted_app(root, "tools/migrate", "migrate", "pnpm-lock.yaml");
    let derived = resolve(root).unwrap();
    assert_eq!(derived.reason, "npm workspaces");
    assert_eq!(directories(&derived), ["packages/api", "packages/web"]);
    assert_eq!(stated(&derived), before);

    // The same holds for a pnpm declaration and for a Deno root.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", "{}");
    write(root, "pnpm-workspace.yaml", "packages:\n  - 'apps/*'\n");
    write(root, "apps/api/package.json", r#"{"name":"api"}"#);
    lockfile_rooted_app(root, "tools/migrate", "migrate", "pnpm-lock.yaml");
    assert_eq!(directories(&resolve(root).unwrap()), ["apps/api"]);

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "deno.json", r#"{"name":"@sample/root"}"#);
    write(root, "main.ts", "export const root = 1;\n");
    lockfile_rooted_app(root, "tools/migrate", "migrate", "pnpm-lock.yaml");
    let derived = resolve(root).unwrap();
    assert_eq!(derived.reason, "Deno manifests");
    assert_eq!(directories(&derived), ["."]);
}

/// Lockfile-rooted packages nested in each other are each proposed, because
/// a file belongs to the deepest service that holds it (carrick#553): the
/// outer one reads its own source and not the inner one's. A package holding
/// no source of its own, only other packages, is not a service: a scan
/// refuses one with nothing in it.
#[test]
fn nested_lockfile_rooted_packages_are_each_proposed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    tooling_root(root);
    // A package and the playground that ships inside it.
    lockfile_rooted_app(root, "widgets", "widgets", "package-lock.json");
    lockfile_rooted_app(
        root,
        "widgets/playground",
        "playground",
        "package-lock.json",
    );
    // A directory that installs tooling for the two applications under it and
    // holds no source of its own.
    let tooling = r#"{"private":true,"devDependencies":{"npm-run-all":"^4.1.5"}}"#;
    write(root, "platform/package.json", tooling);
    write(root, "platform/package-lock.json", "{}");
    lockfile_rooted_app(root, "platform/gateway", "gateway", "yarn.lock");
    lockfile_rooted_app(root, "platform/ledger", "ledger", "yarn.lock");
    // The same manifest over source of its own: a service for that source.
    write(root, "studio/package.json", tooling);
    write(root, "studio/package-lock.json", "{}");
    write(root, "studio/editor.ts", "export const editor = 1;\n");
    lockfile_rooted_app(root, "studio/preview", "preview", "yarn.lock");

    let derived = resolve(root).unwrap();
    assert_eq!(
        directories(&derived),
        [
            "platform/gateway",
            "platform/ledger",
            "studio",
            "studio/preview",
            "widgets",
            "widgets/playground"
        ]
    );
    assert_eq!(read_by(root, &derived, "widgets"), ["widgets/src/main.ts"]);
    assert_eq!(
        read_by(root, &derived, "widgets/playground"),
        ["widgets/playground/src/main.ts"]
    );
    assert_eq!(read_by(root, &derived, "studio"), ["studio/editor.ts"]);
    assert_every_file_is_read_once(root, &derived);
    assert!(derived.warnings.is_empty(), "{:?}", derived.warnings);
}

/// Applications started from one template share a package name. That was
/// never an error for the single service this repository used to be, so each
/// takes its directory instead.
#[test]
fn lockfile_rooted_apps_sharing_a_package_name_are_named_by_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    lockfile_rooted_app(root, "examples/alpha", "starter", "package-lock.json");
    lockfile_rooted_app(root, "examples/beta", "starter", "package-lock.json");
    lockfile_rooted_app(root, "examples/gamma", "gamma", "package-lock.json");
    let derived = resolve(root).unwrap();
    assert_eq!(
        derived
            .services
            .iter()
            .map(|service| service.service_name.as_deref())
            .collect::<Vec<_>>(),
        [Some("examples/alpha"), Some("examples/beta"), Some("gamma")]
    );
}

/// The command and the scan read the same derivation. The applications sit
/// under `apps/` so the repository itself is what the command is asked about.
#[test]
fn the_derive_command_proposes_lockfile_rooted_apps_as_services() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    tooling_root(root);
    lockfile_rooted_app(root, "apps/web", "@sample/web", "package-lock.json");
    lockfile_rooted_app(root, "apps/api", "@sample/api", "package-lock.json");
    let expected = resolve(root).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_carrick"))
        .args(["derive", "--workspace"])
        .arg(root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual["repos"].as_array().unwrap().len(), 1);
    assert_eq!(actual["repos"][0]["reason"], "lockfile-rooted packages");
    assert_eq!(
        actual["repos"][0]["services"],
        serde_json::to_value(expected.service_documents()).unwrap()
    );
    assert_eq!(actual["repos"][0]["services"][0]["directory"], "apps/api");
    assert_eq!(actual["repos"][0]["services"][1]["directory"], "apps/web");
}
