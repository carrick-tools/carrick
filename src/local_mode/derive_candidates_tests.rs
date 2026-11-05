//! `carrick derive --candidates` (carrick#2232): the env vars and URL origins
//! each service's source reads, over the files a scan of that service reads.

use std::path::Path;

use super::derive;

/// The fixture's files, in the order the first run writes them.
const FIXTURE: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{"name":"shop","private":true,"workspaces":["apps/*","services/*"]}"#,
    ),
    ("apps/web/package.json", r#"{"name":"web","private":true}"#),
    (
        "apps/web/src/api.ts",
        "export async function orders() {\n  const key = import.meta.env.VITE_ANALYTICS_KEY;\n  return fetch(`${process.env.API_URL ?? \"http://localhost:4000\"}/orders`, { headers: { key } });\n}\n",
    ),
    (
        "apps/web/src/Footer.tsx",
        "export function Footer() {\n  return <a href=\"https://docs.vendor.example/start\">Docs</a>;\n}\n",
    ),
    (
        "services/api/package.json",
        r#"{"name":"api","private":true}"#,
    ),
    (
        "services/api/src/db.ts",
        "export const databaseUrl = process.env.DATABASE_URL;\n",
    ),
    (
        "services/api/src/pay.ts",
        "const base = process.env.PAYMENTS_BASE ?? \"https://api.payments.example\";\n\nexport async function charge() {\n  await fetch(\"https://api.payments.example/v1/charges\", { method: \"POST\" });\n  return base;\n}\n",
    ),
    (
        "services/api/src/env.ts",
        "import { z } from \"zod\";\n\nexport const env = z.object({\n  ORDERS_URL: z.string().url().default(\"http://localhost:5000\"),\n});\n",
    ),
    (
        "services/api/src/pay.test.ts",
        "export const testOnly = process.env.TEST_ONLY_URL;\n",
    ),
    (
        "services/api/src/hosts.ts",
        "export async function ping(host: string) {\n  return fetch(`https://${host}/v1`);\n}\n",
    ),
    (
        "services/docs/package.json",
        r#"{"name":"docs","private":true}"#,
    ),
    (
        "services/docs/README.md",
        "# Docs\n\nSee https://docs.vendor.example.\n",
    ),
    ("services/docs/guide.md", "# Guide\n"),
];

/// A git repository holding `files`, written in the order given.
fn write_repo(root: &Path, files: &[(&str, &str)]) {
    for (path, body) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
}

/// The document `derive` prints, with the temporary directory written as
/// `<ROOT>` so a literal can hold it.
fn printed(root: &Path, candidates: bool) -> String {
    let document = derive(Some(root), candidates).unwrap();
    let canonical = root.canonicalize().unwrap();
    document
        .to_string()
        .replace(&canonical.display().to_string(), "<ROOT>")
        .replace(&root.display().to_string(), "<ROOT>")
}

/// The `candidates` object of each service, by service name.
fn candidates_by_service(root: &Path) -> serde_json::Map<String, serde_json::Value> {
    let document = derive(Some(root), true).unwrap();
    document["repos"][0]["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|service| {
            (
                // A single-package repo's one service is unnamed.
                service["serviceName"].as_str().unwrap_or("").to_string(),
                service["candidates"].clone(),
            )
        })
        .collect()
}

/// Each service lists the env vars and origins its own source reads, with
/// the default the source states, and nothing from a test file, a loopback
/// literal or an interpolated host.
#[test]
fn each_service_lists_the_env_vars_and_origins_its_source_reads() {
    let dir = tempfile::tempdir().unwrap();
    write_repo(dir.path(), FIXTURE);

    let expected = serde_json::json!({
        "web": {
            "files_read": 2,
            "env": [
                {
                    "name": "API_URL",
                    "default": "http://localhost:4000",
                    "schema_declared": false,
                    "site_count": 1,
                    "sites": [{
                        "file": "apps/web/src/api.ts",
                        "line": 3,
                        "text": "return fetch(`${process.env.API_URL ?? \"http://localhost:4000\"}/orders`, { headers: { key } });"
                    }]
                },
                {
                    "name": "VITE_ANALYTICS_KEY",
                    "default": null,
                    "schema_declared": false,
                    "site_count": 1,
                    "sites": [{
                        "file": "apps/web/src/api.ts",
                        "line": 2,
                        "text": "const key = import.meta.env.VITE_ANALYTICS_KEY;"
                    }]
                }
            ],
            "origins": [{
                "origin": "https://docs.vendor.example",
                "site_count": 1,
                "sites": [{
                    "file": "apps/web/src/Footer.tsx",
                    "line": 2,
                    "text": "return <a href=\"https://docs.vendor.example/start\">Docs</a>;"
                }]
            }],
            "truncated": { "env": 0, "origins": 0 }
        },
        "api": {
            "files_read": 4,
            "env": [
                {
                    "name": "DATABASE_URL",
                    "default": null,
                    "schema_declared": false,
                    "site_count": 1,
                    "sites": [{
                        "file": "services/api/src/db.ts",
                        "line": 1,
                        "text": "export const databaseUrl = process.env.DATABASE_URL;"
                    }]
                },
                {
                    "name": "ORDERS_URL",
                    "default": "http://localhost:5000",
                    "schema_declared": true,
                    "site_count": 1,
                    "sites": [{
                        "file": "services/api/src/env.ts",
                        "line": 4,
                        "text": "ORDERS_URL: z.string().url().default(\"http://localhost:5000\"),"
                    }]
                },
                {
                    "name": "PAYMENTS_BASE",
                    "default": "https://api.payments.example",
                    "schema_declared": false,
                    "site_count": 1,
                    "sites": [{
                        "file": "services/api/src/pay.ts",
                        "line": 1,
                        "text": "const base = process.env.PAYMENTS_BASE ?? \"https://api.payments.example\";"
                    }]
                }
            ],
            "origins": [{
                "origin": "https://api.payments.example",
                "site_count": 2,
                "sites": [
                    {
                        "file": "services/api/src/pay.ts",
                        "line": 1,
                        "text": "const base = process.env.PAYMENTS_BASE ?? \"https://api.payments.example\";"
                    },
                    {
                        "file": "services/api/src/pay.ts",
                        "line": 4,
                        "text": "await fetch(\"https://api.payments.example/v1/charges\", { method: \"POST\" });"
                    }
                ]
            }],
            "truncated": { "env": 0, "origins": 0 }
        }
        // `services/docs` holds a manifest and no source file, so it is no
        // service and has no candidates (carrick#2298).
    });
    // Reach and units have their own cases (tests/derive_reach_units_test.rs,
    // carrick#2397); what this case pins is unchanged beside them.
    let mut found = candidates_by_service(dir.path());
    for candidates in found.values_mut() {
        let candidates = candidates.as_object_mut().unwrap();
        assert!(candidates.remove("reach").is_some());
        assert!(candidates.remove("directories").is_some());
        let truncated = candidates["truncated"].as_object_mut().unwrap();
        assert!(truncated.remove("directories").is_some());
    }
    assert_eq!(serde_json::Value::Object(found), expected);
}

/// The same tree prints the same bytes, whatever order its files were
/// created in and however often it is read.
#[test]
fn the_same_tree_prints_the_same_bytes() {
    let first = tempfile::tempdir().unwrap();
    write_repo(first.path(), FIXTURE);
    let reversed: Vec<(&str, &str)> = FIXTURE.iter().rev().copied().collect();
    let second = tempfile::tempdir().unwrap();
    write_repo(second.path(), &reversed);

    let once = printed(first.path(), true);
    assert_eq!(once, printed(first.path(), true));
    assert_eq!(once, printed(second.path(), true));
}

/// Nothing inside a service names a path on this machine.
#[test]
fn no_service_string_is_an_absolute_path() {
    fn strings<'a>(value: &'a serde_json::Value, found: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(text) => found.push(text),
            serde_json::Value::Array(items) => items.iter().for_each(|item| strings(item, found)),
            // A unit's `pattern` is anchored at its service's directory
            // (`/src/`), so it starts with `/` and is checked below for the
            // machine's paths only.
            serde_json::Value::Object(map) => map
                .iter()
                .filter(|(key, _)| key.as_str() != "pattern")
                .for_each(|(_, item)| strings(item, found)),
            _ => {}
        }
    }
    let dir = tempfile::tempdir().unwrap();
    write_repo(dir.path(), FIXTURE);
    let document = derive(Some(dir.path()), true).unwrap();
    let rendered = document["repos"][0]["services"].to_string();
    let temp = dir.path().canonicalize().unwrap().display().to_string();
    assert!(!rendered.contains(&temp));
    let mut found = Vec::new();
    strings(&document["repos"][0]["services"], &mut found);
    assert!(found.iter().any(|text| text.ends_with(".ts")), "{found:?}");
    let temp = dir.path().canonicalize().unwrap().display().to_string();
    for text in found {
        assert!(!text.starts_with('/'), "{text}");
        assert!(!text.contains(&temp), "{text}");
        assert!(!text.contains(&dir.path().display().to_string()), "{text}");
    }
}

/// Without `--candidates` the document is the one derive printed before the
/// flag existed, byte for byte: `carrick init` and the cloud runner read it.
///
/// `services/docs` holds no source file, so since carrick#2298 it is no
/// service: the document names it in `notes` and nowhere else.
#[test]
fn without_the_flag_derive_prints_what_it_printed_before() {
    let dir = tempfile::tempdir().unwrap();
    write_repo(dir.path(), FIXTURE);
    let golden = concat!(
        r#"{"missing":[],"parent_proposal":null,"repos":[{"config":{"services":[{"directory":"apps/web","externalDomains":[],"externalEnvVars":[],"internalDomains":[],"internalEnvVars":[],"serviceName":"web"},{"directory":"services/api","externalDomains":[],"externalEnvVars":[],"internalDomains":[],"internalEnvVars":[],"serviceName":"api"}]},"#,
        r#""not_installed":[],"notes":["Workspace packages are proposed as services. Review service boundaries and shared source includes in carrick.json.","skipped_no_source: services/docs"],"path":"<ROOT>","reason":"npm workspaces","#,
        r#""services":[{"bin":false,"deploy_config":[],"directory":"apps/web","exports":false,"externalDomains":[],"externalEnvVars":[],"internalDomains":[],"internalEnvVars":[],"main":false,"private":true,"serviceName":"web","workspace_dependents":[]},{"bin":false,"deploy_config":[],"directory":"services/api","exports":false,"externalDomains":[],"externalEnvVars":[],"internalDomains":[],"internalEnvVars":[],"main":false,"private":true,"serviceName":"api","workspace_dependents":[]}],"skipped_patterns":[],"warnings":[]}],"#,
        r#""repos_added":[],"repos_detected_by":"workspace manifest","repos_excluded":[],"schema":"carrick.derive/0","workspace":"<ROOT>"}"#,
    );
    assert_eq!(printed(dir.path(), false), golden);
}

/// A service reading more env vars than the cap lists the most-read first,
/// then by name, and says how many it cut.
#[test]
fn a_service_past_the_cap_lists_the_most_read_and_counts_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = String::new();
    for index in 0..400 {
        source.push_str(&format!(
            "export const v{index} = process.env.VAR_{index:03};\n"
        ));
    }
    source.push_str("export const again = process.env.VAR_399;\n");
    write_repo(
        dir.path(),
        &[
            ("package.json", r#"{"name":"many","private":true}"#),
            ("src/many.ts", &source),
        ],
    );
    let services = candidates_by_service(dir.path());
    assert_eq!(services.len(), 1, "{services:?}");
    let candidates = services.values().next().unwrap();
    let env = candidates["env"].as_array().unwrap();
    assert_eq!(env.len(), 300);
    assert_eq!(candidates["truncated"]["env"], 100);
    assert_eq!(env[0]["name"], "VAR_399");
    assert_eq!(env[0]["site_count"], 2);
    assert_eq!(env[1]["name"], "VAR_000");
    assert_eq!(env[299]["name"], "VAR_298");
}

/// A repository with a `carrick.json` gets candidates for the services it
/// declares, each from its own directory.
#[test]
fn a_declared_service_gets_candidates_from_its_own_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_repo(
        dir.path(),
        &[
            (
                "carrick.json",
                r#"{"services":[{"serviceName":"orders","directory":"orders"},{"serviceName":"billing","directory":"billing"}]}"#,
            ),
            (
                "orders/index.ts",
                "export const url = process.env.ORDERS_DB;\n",
            ),
            (
                "billing/index.ts",
                "export const key = process.env.BILLING_KEY;\nfetch(\"https://billing.vendor.example/v2\");\n",
            ),
        ],
    );
    let services = candidates_by_service(dir.path());
    let names = |service: &str, list: &str, field: &str| -> Vec<String> {
        services[service][list]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry[field].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names("orders", "env", "name"), ["ORDERS_DB"]);
    assert!(names("orders", "origins", "origin").is_empty());
    assert_eq!(names("billing", "env", "name"), ["BILLING_KEY"]);
    assert_eq!(
        names("billing", "origins", "origin"),
        ["https://billing.vendor.example"]
    );
    assert_eq!(services["orders"]["files_read"], 1);
}

#[test]
fn candidates_is_a_derive_option_only() {
    let args = |input: &[&str]| input.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        super::parse(&args(&["derive", "--json", "--candidates"])),
        Some(Ok(super::LocalCommand::Derive {
            workspace: None,
            candidates: true,
        }))
    );
    assert_eq!(
        super::parse(&args(&["status", "--candidates"])),
        Some(Err(
            "unknown option for `carrick status`: --candidates".to_string()
        ))
    );
}
