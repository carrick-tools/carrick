//! carrick#806: the type layer addresses a call site by span, and the units the
//! scanner counts in are not the units the sidecar reads.
//!
//! A span the scanner records is an SWC position: a UTF-8 byte offset counted
//! from one. The sidecar resolves it against a ts-morph position, which
//! TypeScript counts in UTF-16 code units from zero, and it resolves by
//! containment — so a span in the wrong numbering does not name the site, it
//! names whatever encloses it, and the answer that comes back is about a
//! different expression. Nothing in the output says so.
//!
//! On an ASCII file the two numberings differ only by the base. From a file's
//! first multi-byte character onwards they diverge cumulatively as well, and a
//! site far enough down the file misses by tens of positions.
//!
//! Both tests below run over the same source twice: once as written, once under
//! a header of accented prose that nothing in the pipeline reads. The answer
//! must not depend on which one it is.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::{DataCallResult, FileAnalysisResult};
use carrick::agents::file_orchestrator::FileOrchestrator;
use carrick::config::Config;
use carrick::services::type_sidecar::{InferKind, InferRequestItem, TypeSidecar};
use carrick::swc_scanner::SWC_SPAN_BASE;
use carrick::url_normalizer::UrlNormalizer;
use tempfile::TempDir;

/// The service. One typed client, one call on it, and the payload declared in
/// the repo itself — an installed dependency would answer `any` on a CI
/// checkout and `any` proves nothing about which node was read.
const CLIENT_TS: &str = r#"export interface Widget {
  id: string;
  label: string;
}

export interface WidgetClient {
  get(path: string): Promise<Widget>;
}

export const client: WidgetClient = {
  get: async (_path: string) => ({ id: 'w1', label: 'first' }),
};

export async function loadWidget(): Promise<Widget> {
  const widget = await client.get('https://widgets.example.com/widgets');
  return widget;
}
"#;

/// The same source under a header of multi-byte prose. Every character in it
/// costs one more byte than it costs a UTF-16 unit, so the call below sits far
/// past the two positions of slack the sidecar's span lookup allows.
const HEADER: &str = "// Requisições à API: os plantões são carregados aqui.\n\
// Não há nada de especial nestas linhas além dos acentos e das cedilhas.\n\
// Uma terceira linha acentuada, só para empurrar o deslocamento além da folga.\n";

/// The call the data-call row is recorded at.
const CALL_TEXT: &str = "client.get('https://widgets.example.com/widgets')";

const ALIAS_FRAGMENT: &str = "_Response_Call";

fn write_repo(root: &Path, source: &str) {
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::write(
        root.join("package.json"),
        r#"{ "name": "span-units-fixture", "version": "1.0.0" }"#,
    )
    .expect("package.json");
    fs::write(
        root.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .expect("tsconfig");
    fs::write(root.join("src/client.ts"), source).expect("client.ts");
}

/// Where the call sits, in each of the two numberings: the SWC position the
/// scanner records, and the ts-morph position the sidecar reads.
struct Site {
    swc_span: (u32, u32),
    ts_morph_span: (u32, u32),
    line: i32,
}

fn locate_call(source: &str) -> Site {
    let byte_start = source.find(CALL_TEXT).expect("the fixture makes the call");
    let byte_end = byte_start + CALL_TEXT.len();
    let units = |byte_offset: usize| -> u32 {
        source[..byte_offset]
            .chars()
            .map(|c| c.len_utf16() as u32)
            .sum()
    };
    Site {
        swc_span: (
            byte_start as u32 + SWC_SPAN_BASE,
            byte_end as u32 + SWC_SPAN_BASE,
        ),
        ts_morph_span: (units(byte_start), units(byte_end)),
        line: source[..byte_start].lines().count() as i32,
    }
}

/// The analyzer result for a call the model did not describe: a row the
/// deterministic layer states on its own, whose only locator is its span. That
/// is the live shape this issue is about — `collect_type_requests` tries the
/// model's expression text first, so the span is what a row without one falls
/// back to.
fn analyzer_result(client_file: &str, site: &Site) -> HashMap<String, FileAnalysisResult> {
    let call = DataCallResult {
        call_kind: None,
        candidate_id: format!("span:{}-{}", site.swc_span.0, site.swc_span.1),
        line_number: site.line,
        target: "https://widgets.example.com/widgets".to_string(),
        method: Some("GET".to_string()),
        pattern_matched: "client.get(".to_string(),
        call_expression_span_start: Some(site.swc_span.0),
        call_expression_span_end: Some(site.swc_span.1),
        call_expression_text: None,
        call_expression_line: None,
        payload_expression_text: None,
        payload_expression_line: None,
        primary_type_symbol: None,
        type_import_source: None,
        loopback_default_url: None,
        base: None,
        consumers_not_resolved: None,
        resolution_source: None,
    };

    let mut file_results = HashMap::new();
    file_results.insert(
        client_file.to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![],
            data_calls: vec![call],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        },
    );
    file_results
}

/// Run the scanner's own locator emission over the fixture and return the one
/// span-located request it produces.
fn emitted_infer_item(repo: &Path, source: &str) -> InferRequestItem {
    let client_file = repo.join("src/client.ts").to_string_lossy().to_string();
    let file_results = analyzer_result(&client_file, &locate_call(source));

    let orchestrator = FileOrchestrator::new(AgentService::new());
    let mount_graph = orchestrator.build_mount_graph(
        &file_results,
        &UrlNormalizer::default_permissive(),
        Path::new(""),
        Path::new(""),
    );
    let (_explicit, infer, _inline) = orchestrator.collect_type_requests(
        &file_results,
        &repo.to_string_lossy(),
        &mount_graph,
        &Config::default(),
    );

    infer
        .into_iter()
        .find(|item| {
            item.infer_kind == InferKind::CallResult
                && item
                    .alias
                    .as_deref()
                    .is_some_and(|alias| alias.contains(ALIAS_FRAGMENT))
        })
        .expect("a data call with no expression text must fall back to its span")
}

fn is_node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sidecar_path() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    path.exists().then_some(path)
}

/// What the sidecar answers for the fixture's call, through the request the
/// scanner really emits. `None` when the sidecar is not available to run.
fn inferred_call_type(source: &str) -> Option<String> {
    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return None;
    }
    let sidecar_path = sidecar_path()?;

    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("service");
    write_repo(&root, source);
    let item = emitted_infer_item(&root, source);

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(&root, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");
    let result = sidecar
        .resolve_all_types(&[], std::slice::from_ref(&item), None)
        .expect("infer request");

    Some(
        result
            .inferred_types
            .first()
            .map(|inferred| inferred.type_string.clone())
            .unwrap_or_else(|| "<nothing inferred>".to_string()),
    )
}

/// The number on the wire, before any sidecar runs: a span request carries the
/// position the sidecar counts in, not the one the scanner stored.
///
/// Asserted on the ASCII source too, and not only on the accented one. The
/// numberings differ there by the base alone, which the span lookup for a CALL
/// tolerates — but the lookups behind `response_body`, `request_body` and
/// `expression` require exact containment, and one position of overshoot is
/// enough to exclude the node they name and hand the answer to its parent.
#[test]
fn a_span_request_carries_the_sidecars_position_not_the_scanners() {
    let temp = TempDir::new().expect("temp dir");

    for (name, source) in [
        ("ascii", CLIENT_TS.to_string()),
        ("accented", format!("{HEADER}{CLIENT_TS}")),
    ] {
        let root = temp.path().join(name);
        write_repo(&root, &source);

        let site = locate_call(&source);
        let item = emitted_infer_item(&root, &source);

        assert_eq!(
            (item.span_start, item.span_end),
            (Some(site.ts_morph_span.0), Some(site.ts_morph_span.1)),
            "the {name} request must name the call in ts-morph positions"
        );
    }

    let accented = locate_call(&format!("{HEADER}{CLIENT_TS}"));
    assert!(
        accented.swc_span.0 - accented.ts_morph_span.0 > 2,
        "the accented fixture must put the call far enough below its header to \
         outrun the sidecar's slack, or the test below asserts nothing (drift {})",
        accented.swc_span.0 - accented.ts_morph_span.0
    );
}

/// The same call, twice, through the real sidecar: what the compiler says about
/// a site cannot depend on how many bytes of prose sit above it.
#[test]
fn a_call_below_multi_byte_text_infers_the_same_type_as_its_ascii_twin() {
    let Some(ascii) = inferred_call_type(CLIENT_TS) else {
        return;
    };
    let Some(accented) = inferred_call_type(&format!("{HEADER}{CLIENT_TS}")) else {
        return;
    };

    assert_eq!(
        ascii, "Widget",
        "the ASCII twin reads the call's own payload type"
    );
    assert_eq!(
        accented, ascii,
        "the site below the accented header reads the same type as its twin"
    );
}
