//! Tests for URL normalization and env-var alias resolution
//!
//! This tests that:
//! 1. Template literal expressions like ${varName} are properly converted to :varName style
//! 2. Env-var bases aliased through local consts or imported config objects resolve
//!    back to the real `process.env` name so classification and cross-repo matching work

use carrick::url_normalizer::UrlNormalizer;
use std::fs;
use std::io::Write;
use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_visit::VisitWith;
use tempfile::tempdir;

/// The production path turns template-literal path parameters into `:param`
/// segments: the normalizer every call target passes through. (The retired
/// call-site extractor did this beside it and nothing reached it.)
#[test]
fn test_normalizer_turns_template_params_into_path_params() {
    let normalizer = UrlNormalizer::default_permissive();
    let normalized = normalizer.normalize("${process.env.API_URL}/users/${userId}");

    assert!(
        !normalized.path.contains("${"),
        "path should not contain template literal syntax. Got: {}",
        normalized.path
    );
    assert_eq!(
        normalized.path, "/users/:userId",
        "path parameter should be :param style"
    );
}

#[test]
fn test_const_aliased_env_var_resolves_to_real_name() {
    use carrick::config::Config;
    use carrick::env_alias::{EnvAliasExtractor, resolve_target_env_alias};
    use carrick::parser::parse_file;
    use carrick::url_normalizer::UrlNormalizer;

    // The exact pattern from issue #218 (payments-svc/clients/orders.client.ts).
    let source = r#"
const ORDERS_BASE = process.env.ORDERS_SERVICE_URL ?? "http://localhost:3001";

export async function getOrder(orderId: number) {
  return fetch(`${ORDERS_BASE}/orders/${orderId}`);
}
"#;

    let temp_dir = tempdir().expect("Failed to create temp dir");
    let file_path = temp_dir.path().join("orders.client.ts");
    let mut file = fs::File::create(&file_path).expect("Failed to create temp file");
    file.write_all(source.as_bytes()).expect("write temp file");

    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Auto, true, false, Some(cm.clone()));
    let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

    // 1. The per-file alias map links the local const to the real env var.
    let aliases = EnvAliasExtractor::build(&module);
    assert_eq!(
        aliases.get("ORDERS_BASE").map(String::as_str),
        Some("ORDERS_SERVICE_URL"),
        "alias map should resolve the const to the process.env name"
    );

    // 2. The call target the LLM emits, rewritten through the alias map.
    let target = "${ORDERS_BASE}/orders/${orderId}";
    let rewritten = resolve_target_env_alias(target, &aliases)
        .expect("leading const alias should be rewritten");
    assert_eq!(
        rewritten,
        "${process.env.ORDERS_SERVICE_URL}/orders/${orderId}"
    );

    // 3. With ORDERS_SERVICE_URL declared internal, the rewritten target now
    //    classifies as internal and yields the producer-matchable path. The
    //    original (unaliased) target does NOT — that is the bug.
    let config = Config {
        internal_env_vars: ["ORDERS_SERVICE_URL"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ..Default::default()
    };
    let normalizer = UrlNormalizer::new(&config);

    let resolved = normalizer.normalize(&rewritten);
    assert_eq!(resolved.path, "/orders/:orderId");
    assert!(
        resolved.is_internal,
        "rewritten target must classify as internal via the real env var"
    );

    let unresolved = normalizer.normalize(target);
    assert!(
        !unresolved.is_internal,
        "the un-rewritten const-aliased target should NOT classify as internal (this is #218)"
    );
}

/// Issue #218, cross-file config-object shape: a consumer that builds its URLs
/// from an *imported* config-object property must resolve the property back to
/// the real `process.env` name, exactly as a local direct alias does.
///
/// Mirrors the corpus-3 ops-console shape with generic fixture names:
///
/// ```ts
/// // config.ts
/// export const config = {
///   catalogUrl: process.env.CATALOG_URL ?? "http://localhost:4001",
/// };
/// // consumer.ts
/// import { config } from "./config";
/// const client = makeClient(config.catalogUrl);
/// ```
///
/// The file analyzer emits the call target with the config property verbatim
/// (`${config.catalogUrl}/api/v2/products/${id}`). Before the fix, env-alias
/// resolution only tracked direct local `const X = process.env.NAME` bindings,
/// so the base stayed verbatim in the call key, classification lost the env-var
/// name, and the cross-repo edge never formed.
#[test]
fn test_imported_config_object_property_resolves_env_var() {
    use carrick::agents::file_orchestrator::FileOrchestrator;
    use carrick::config::Config;
    use carrick::env_alias::{EnvAliasExtractor, EnvAliasMap, resolve_target_env_alias};
    use carrick::parser::parse_file;
    use carrick::url_normalizer::UrlNormalizer;
    use carrick::visitor::ImportSymbolExtractor;
    use std::collections::HashMap;
    use std::path::PathBuf;

    let temp_dir = tempdir().expect("Failed to create temp dir");

    // Config module: env bases read here, not inline at call sites.
    let config_source = r#"
export const config = {
  ordersApiUrl: process.env.ORDERS_API_URL ?? "http://localhost:4003",
  catalogUrl: process.env.CATALOG_URL ?? "http://localhost:4001",
};
"#;
    fs::write(temp_dir.path().join("config.ts"), config_source).expect("write config.ts");

    // Consumer: imports the config object and passes a property as the client base.
    let consumer_source = r#"
import { makeClient } from "./lib/apiClient";
import { config } from "./config";

const catalogClient = makeClient(config.catalogUrl);

export async function updateProduct(id: string, patch: object) {
  return catalogClient.patch(`/api/v2/products/${id}`, patch);
}
"#;
    let consumer_path = temp_dir.path().join("consumer.ts");
    fs::write(&consumer_path, consumer_source).expect("write consumer.ts");

    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Auto, true, false, Some(cm.clone()));
    let module = parse_file(&consumer_path, &cm, &handler).expect("parsed consumer module");

    // 1. The consumer's own alias map is empty (no local process.env reads)...
    let mut aliases = EnvAliasExtractor::build(&module);
    assert!(aliases.is_empty(), "consumer has no local env aliases");

    // ...until the cross-file pass follows the import graph to the config
    // module and resolves its object-literal properties.
    let mut import_extractor = ImportSymbolExtractor::new();
    module.visit_with(&mut import_extractor);
    let mut cache: HashMap<PathBuf, EnvAliasMap> = HashMap::new();
    FileOrchestrator::merge_cross_file_env_aliases(
        &mut aliases,
        &consumer_path,
        &import_extractor.imported_symbols,
        &mut cache,
        &cm,
        &handler,
    );
    assert_eq!(
        aliases.get("config.catalogUrl").map(String::as_str),
        Some("CATALOG_URL"),
        "imported config-object property must resolve to the real env var"
    );
    assert_eq!(
        aliases.get("config.ordersApiUrl").map(String::as_str),
        Some("ORDERS_API_URL"),
    );

    // 2. The call target the LLM emits (wrapper base resolved to the call-site
    //    argument, #370), rewritten through the augmented alias map.
    let target = "${config.catalogUrl}/api/v2/products/${id}";
    let rewritten = resolve_target_env_alias(target, &aliases)
        .expect("leading imported-config-property base should be rewritten");
    assert_eq!(
        rewritten,
        "${process.env.CATALOG_URL}/api/v2/products/${id}"
    );

    // 3. With CATALOG_URL declared internal, the rewritten target classifies as
    //    internal and yields the producer-matchable path; the verbatim target
    //    does not — that is the regression this fix closes.
    let config = Config {
        internal_env_vars: ["ORDERS_API_URL", "CATALOG_URL"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ..Default::default()
    };
    let normalizer = UrlNormalizer::new(&config);

    let resolved = normalizer.normalize(&rewritten);
    assert_eq!(resolved.path, "/api/v2/products/:id");
    assert!(
        resolved.is_internal,
        "rewritten target must classify as internal via the real env var"
    );
}

/// Issue #218, corpus-3 regression pin: drive the *actual* committed
/// `xrepo-corpus-3` ops-console fixture through the deterministic post-LLM
/// chain (cross-file alias merge → target rewrite → normalization) and assert
/// both HTTP edges recover their declared env-var names — with the corpus
/// config exactly as committed. Both edges stopped matching when an
/// unclassified `${var}` base began staying verbatim in the call key; this
/// pins that they classify internal again.
#[test]
fn test_corpus3_ops_console_edges_resolve_via_imported_config() {
    use carrick::agents::file_orchestrator::FileOrchestrator;
    use carrick::config::Config;
    use carrick::env_alias::{EnvAliasExtractor, EnvAliasMap, resolve_target_env_alias};
    use carrick::parser::parse_file;
    use carrick::url_normalizer::UrlNormalizer;
    use carrick::visitor::ImportSymbolExtractor;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    let ops_console =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xrepo-corpus-3/ops-console");

    // The corpus repo's committed carrick.json declares these internal.
    let config = Config {
        internal_env_vars: ["ORDERS_API_URL", "CATALOG_URL"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ..Default::default()
    };
    let normalizer = UrlNormalizer::new(&config);

    // (consumer file, LLM-emitted target shape, expected env var, expected path)
    let edges = [
        (
            "src/orders.ts",
            "${config.ordersApiUrl}/orders/${orderId}/timeline",
            "ORDERS_API_URL",
            "/orders/:orderId/timeline",
        ),
        (
            "src/products.ts",
            "${config.catalogUrl}/api/v2/products/${id}",
            "CATALOG_URL",
            "/api/v2/products/:id",
        ),
    ];

    for (file, target, env_var, expected_path) in edges {
        let consumer_path = ops_console.join(file);
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Auto, true, false, Some(cm.clone()));
        let module = parse_file(&consumer_path, &cm, &handler).expect("parsed fixture module");

        let mut aliases = EnvAliasExtractor::build(&module);
        let mut import_extractor = ImportSymbolExtractor::new();
        module.visit_with(&mut import_extractor);
        let mut cache: HashMap<PathBuf, EnvAliasMap> = HashMap::new();
        FileOrchestrator::merge_cross_file_env_aliases(
            &mut aliases,
            &consumer_path,
            &import_extractor.imported_symbols,
            &mut cache,
            &cm,
            &handler,
        );

        let rewritten = resolve_target_env_alias(target, &aliases).unwrap_or_else(|| {
            panic!("{file}: imported-config base in `{target}` should be rewritten")
        });
        assert_eq!(
            rewritten,
            format!(
                "${{process.env.{env_var}}}{}",
                &target[target.find('}').unwrap() + 1..]
            ),
        );

        let resolved = normalizer.normalize(&rewritten);
        assert_eq!(resolved.path, expected_path, "{file}");
        assert!(
            resolved.is_internal,
            "{file}: edge must classify internal via {env_var}"
        );
    }
}
