use crate::cloud_storage::{ManifestRole, ManifestTypeKind};
use crate::operation::OperationKey;
use swc_common::errors::Handler;
use swc_common::{SourceMap, sync::Lrc};
use swc_ecma_ast::{Decl, ModuleDecl, ModuleItem, Stmt};

pub fn normalize_manifest_method(method: &str) -> String {
    let trimmed = method.trim();
    if trimmed.is_empty() {
        "UNKNOWN".to_string()
    } else {
        trimmed.to_uppercase()
    }
}

pub fn is_http_method(method: &str) -> bool {
    matches!(
        method.trim().to_uppercase().as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" | "CONNECT" | "TRACE"
    )
}

pub fn build_display_name(key: &OperationKey, type_kind: &str) -> String {
    let kind = if type_kind.is_empty() {
        type_kind.to_string()
    } else {
        let mut chars = type_kind.chars();
        match chars.next() {
            Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str().to_lowercase()),
            None => String::new(),
        }
    };
    format!("{} → {}", key, kind)
}

/// The alias for one operation, keyed on the operation alone. Every caller that
/// can name the SITE the type belongs to should use
/// [`build_manifest_type_alias_with_site_id`] instead: an operation key is not
/// unique to one place in the source on either side (carrick#718).
pub fn build_manifest_type_alias(
    key: &OperationKey,
    role: ManifestRole,
    type_kind: ManifestTypeKind,
) -> String {
    let role_label = match role {
        ManifestRole::Producer => "producer",
        ManifestRole::Consumer => "consumer",
    };
    let type_label = match type_kind {
        ManifestTypeKind::Request => "Request",
        ManifestTypeKind::Response => "Response",
    };

    let hash_input = format!("{}|{}|{}", key.canonical(), role_label, type_label);
    let hash = fnv1a_hash(&hash_input);

    format!("Endpoint_{:016x}_{}", hash, type_label)
}

/// Reduce a source path to its repo-relative form for hashing: strip the
/// repo-root prefix when present, then any leading `./`. Mirrors the cache-key
/// normalization in `normalize_file_results_keys` so a full-scan absolute key
/// (`/abs/repo/src/api.ts`) and an incremental repo-relative key (`src/api.ts`)
/// reduce to the same string. A path outside the root passes through unchanged.
fn repo_relative_source_path<'a>(file_path: &'a str, repo_root: &str) -> &'a str {
    let root = repo_root.trim_end_matches('/');
    let stripped = if root.is_empty() || root == "." {
        file_path
    } else {
        file_path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(file_path)
    };
    stripped.strip_prefix("./").unwrap_or(stripped)
}

/// Hash a consumer call site into the 16-hex id embedded in
/// site-suffixed aliases. The id is a join key across the manifest,
/// SymbolRequest, and infer-request sides, so every producer of it must call
/// this function with the same `(path, line, key)` triple.
///
/// "Site" means the call site for a consumer and the declaration site for a
/// producer (carrick#718). Both need one for the same reason: an operation key
/// is not unique to one place in the source. A consumer fans in — many call
/// sites, one route — and a producer can too, because two modules legitimately
/// serve one `(method, path)`: a pathless layout and the page beneath it, an
/// `index` module and its sibling. Without the site they share an alias, and
/// one resolved definition is reported for both.
///
/// The path is reduced to its repo-relative form before hashing (issue #355):
/// hashing the absolute path made every `_Call<id>` alias machine-specific,
/// which broke byte-compared goldens across machines and any future
/// output-determinism guarantee. Relativizing INSIDE this function (rather
/// than at call sites) keeps the id identical at every join site regardless of
/// whether the caller holds an absolute full-scan key or a repo-relative
/// incremental key.
pub fn build_site_id(
    file_path: &str,
    line_number: u32,
    key: &OperationKey,
    repo_root: &str,
) -> String {
    let relative = repo_relative_source_path(file_path, repo_root);
    let hash_input = format!("{}|{}|{}", relative, line_number, key.canonical());
    format!("{:016x}", fnv1a_hash(&hash_input))
}

/// The alias for one operation AT ONE SITE. `site_id` comes from
/// [`build_site_id`]; `None` leaves the key-only alias, which is what the
/// non-HTTP protocols still use on the producer side (their producers are
/// keyed by event, field or topic name and no collision has been observed).
///
/// The suffix is spelled for the role, because the two sites are different
/// things: `_Call<id>` is where a consumer calls the operation, `_At<id>` is
/// where a producer declares it. Nothing parses either spelling — the manifest
/// entry carries the alias and every reader takes it whole — so the spelling is
/// for the human reading a bundled `.d.ts` or an MCP result.
pub fn build_manifest_type_alias_with_site_id(
    key: &OperationKey,
    role: ManifestRole,
    type_kind: ManifestTypeKind,
    site_id: Option<&str>,
) -> String {
    let base = build_manifest_type_alias(key, role, type_kind);
    let marker = match role {
        ManifestRole::Consumer => "Call",
        ManifestRole::Producer => "At",
    };
    match site_id {
        Some(id) if !id.trim().is_empty() => format!("{}_{}{}", base, marker, id.trim()),
        _ => base,
    }
}

pub fn parse_file_location(location: &str) -> (String, u32) {
    let segments: Vec<&str> = location.split(':').collect();
    if segments.len() < 2 {
        return (location.to_string(), 1);
    }

    let mut line_number = None;
    let mut cut_index = segments.len();

    if let Ok(last_num) = segments[segments.len() - 1].parse::<u32>() {
        if let Ok(second_last_num) = segments[segments.len() - 2].parse::<u32>() {
            line_number = Some(second_last_num);
            cut_index = segments.len().saturating_sub(2);
        } else {
            line_number = Some(last_num);
            cut_index = segments.len().saturating_sub(1);
        }
    }

    let file_path = if cut_index < segments.len() {
        segments[..cut_index].join(":")
    } else {
        location.to_string()
    };

    let line_number = match line_number {
        Some(0) | None => 1,
        Some(value) => value,
    };

    (file_path, line_number)
}

fn fnv1a_hash(input: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET_BASIS;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The 1-based line at which `file` DECLARES `symbol`, or `None` when it
/// declares no such type (carrick#649).
///
/// Type-space declarations only — an interface, a type alias, a class, or an
/// enum, exported or not — because that is what a manifest anchor names. A
/// re-export (`export { Foo } from "./foo"`) is deliberately not a declaration
/// here: the file states where the symbol comes from, not what it is, and
/// following the chain would need the module resolution the sidecar does. The
/// answer is then `None`, which is the honest one.
pub fn type_declaration_line(
    file: &std::path::Path,
    symbol: &str,
    cm: &Lrc<SourceMap>,
    handler: &Handler,
) -> Option<u32> {
    let module = crate::parser::parse_file(file, cm, handler)?;
    module.body.iter().find_map(|item| {
        let decl = match item {
            ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
            _ => return None,
        };
        let span = match decl {
            Decl::TsInterface(d) if d.id.sym == *symbol => d.span,
            Decl::TsTypeAlias(d) if d.id.sym == *symbol => d.span,
            Decl::Class(d) if d.ident.sym == *symbol => d.class.span,
            Decl::TsEnum(d) if d.id.sym == *symbol => d.span,
            _ => return None,
        };
        u32::try_from(cm.lookup_char_pos(span.lo).line).ok()
    })
}

// ============================================================================
// The bundled `.d.ts` and its unresolved-alias placeholder
// ============================================================================

/// Trailing marker stamped onto every `= unknown` alias statement Carrick
/// itself writes into the bundled `.d.ts`, for one meaning: **no shape reached
/// the bundle for this alias**.
///
/// Two writers stamp it, for the same fact seen at two moments —
/// [`append_alias_declaration`] when v1 was asked for an alias and could not
/// answer, and [`append_missing_aliases`] when an alias never reached the
/// bundle at all. Without the marker the first of those was indistinguishable
/// from a developer-authored `type X = unknown` in a real API type, and a
/// reader promoted it to a defined type (carrick#780).
///
/// A developer's own `= unknown` carries no marker and is therefore never
/// mistaken for a placeholder: its edge keeps its resolved state rather than
/// being silently downgraded (#244).
pub const MISSING_ALIAS_MARKER: &str = "// carrick:missing-alias";

/// Whether the bundled `.d.ts` carries any type-space declaration of `alias`
/// (type, interface, class, enum, or namespace) — including the placeholder,
/// which is a declaration like any other. Ask
/// [`dts_alias_is_trivially_unknown`] to tell the two apart.
pub fn dts_defines_alias(content: &str, alias: &str) -> bool {
    let escaped = regex::escape(alias);
    let pattern = format!(r"\b(type|interface|class|enum|namespace)\s+{}\b", escaped);
    match regex::Regex::new(&pattern) {
        Ok(re) => re.is_match(content),
        Err(_) => false,
    }
}

/// Whether the only statement the bundle carries for `alias` is a
/// Carrick-injected placeholder — identified by [`MISSING_ALIAS_MARKER`] on the
/// same line, never by the `= unknown` text alone.
pub fn dts_alias_is_trivially_unknown(content: &str, alias: &str) -> bool {
    let escaped = regex::escape(alias);
    let marker = regex::escape(MISSING_ALIAS_MARKER);
    // Anchor on the exact form the writers emit:
    //   export type <alias> = unknown; // carrick:missing-alias
    // The optional `export`, generics, and modifiers are tolerated, but the
    // trailing marker on the same line is what actually identifies it as ours.
    let pattern = format!(r"\btype\s+{escaped}\b[^\n]*=\s*unknown\s*;[^\n]*{marker}");
    match regex::Regex::new(&pattern) {
        Ok(re) => re.is_match(content),
        Err(_) => false,
    }
}

/// Append `export type <alias> = <type_string>;` to a bundled `.d.ts`.
///
/// A bare `unknown` type string is not a shape — it is the writer saying it has
/// none — so the statement is stamped with [`MISSING_ALIAS_MARKER`]. Every
/// alias statement Carrick composes goes through here so that the marker
/// cannot be forgotten at one of the sites; the bundler's own extracted
/// declarations (which carry real source text) do not.
pub fn append_alias_declaration(dts: &mut String, alias: &str, type_string: &str) {
    let body = type_string.trim().trim_end_matches(';');
    if !dts.is_empty() && !dts.ends_with('\n') {
        dts.push('\n');
    }
    dts.push_str("export type ");
    dts.push_str(alias);
    dts.push_str(" = ");
    dts.push_str(body);
    dts.push(';');
    if body.trim() == "unknown" {
        dts.push(' ');
        dts.push_str(MISSING_ALIAS_MARKER);
    }
    dts.push('\n');
}

/// Replace an alias's placeholder statement with a real type, marker and all.
///
/// Returns `false` when the bundle has no placeholder for `alias` (either it
/// carries a real declaration already, or it says nothing about the alias), so
/// the caller can append instead. The marker goes with the `= unknown` it
/// described: leaving it behind would leave the bundle stating "no shape
/// reached the bundle" on a line that now carries one.
pub fn replace_unresolved_alias(content: &mut String, alias: &str, type_string: &str) -> bool {
    let escaped = regex::escape(alias);
    let marker = regex::escape(MISSING_ALIAS_MARKER);
    let pattern = format!(r"export\s+type\s+{escaped}\s*=\s*unknown\s*;[^\S\n]*(?:{marker})?");
    let Ok(re) = regex::Regex::new(&pattern) else {
        return false;
    };
    if !re.is_match(content) {
        return false;
    }
    let replacement = format!(
        "export type {} = {};",
        alias,
        type_string.trim().trim_end_matches(';')
    );
    *content = re.replace(content, replacement.as_str()).to_string();
    true
}

/// Append a marked `= unknown` placeholder for every manifest alias the bundle
/// says nothing about, so that "no shape reached the bundle" is stated for the
/// alias rather than left to a reader to infer from the alias's absence.
pub fn append_missing_aliases(
    content: String,
    manifest: Option<&Vec<crate::cloud_storage::TypeManifestEntry>>,
) -> String {
    let Some(entries) = manifest else {
        return content;
    };

    let mut updated = content;
    let mut seen = std::collections::HashSet::new();

    for entry in entries {
        if !seen.insert(entry.type_alias.clone()) {
            continue;
        }

        if dts_defines_alias(&updated, &entry.type_alias) {
            continue;
        }

        append_alias_declaration(&mut updated, &entry.type_alias, "unknown");
    }

    updated
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_common::errors::ColorConfig;

    /// Write a source file and ask where it declares `symbol`.
    fn declaration_line(source: &str, symbol: &str) -> Option<u32> {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file = tmp_dir.path().join("input.ts");
        std::fs::write(&file, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        type_declaration_line(&file, symbol, &cm, &handler)
    }

    #[test]
    fn every_type_space_declaration_shape_reports_its_line() {
        let source = "export interface LedgerEntry {\n  id: string;\n}\n\
                      type LedgerId = string;\n\
                      export class LedgerClient {}\n\
                      export enum LedgerState { Open }\n";
        assert_eq!(declaration_line(source, "LedgerEntry"), Some(1));
        assert_eq!(
            declaration_line(source, "LedgerId"),
            Some(4),
            "a declaration the file does not export is still where the type lives"
        );
        assert_eq!(declaration_line(source, "LedgerClient"), Some(5));
        assert_eq!(declaration_line(source, "LedgerState"), Some(6));
    }

    #[test]
    fn a_re_export_is_not_a_declaration() {
        assert_eq!(
            declaration_line(r#"export { LedgerEntry } from "./ledger";"#, "LedgerEntry"),
            None,
            "the file says where the symbol comes from, not what it is"
        );
        assert_eq!(
            declaration_line(r#"export * from "./ledger";"#, "LedgerEntry"),
            None
        );
    }

    #[test]
    fn a_value_binding_is_not_a_type_declaration() {
        assert_eq!(
            declaration_line(r#"export const LedgerEntry = { id: "" };"#, "LedgerEntry"),
            None,
            "a manifest anchor names a type, and a const is not one"
        );
    }

    #[test]
    fn a_symbol_the_file_does_not_declare_reports_nothing() {
        assert_eq!(
            declaration_line("export interface Other { id: string }", "LedgerEntry"),
            None
        );
    }

    #[test]
    fn test_build_manifest_type_alias_with_site_id() {
        let key = OperationKey::http("GET", "/users");
        let base =
            build_manifest_type_alias(&key, ManifestRole::Consumer, ManifestTypeKind::Response);
        let call_id = build_site_id("src/service.ts", 12, &key, ".");
        let with_call = build_manifest_type_alias_with_site_id(
            &key,
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            Some(&call_id),
        );

        assert_ne!(base, with_call);
        assert!(with_call.contains("_Call"));
    }

    /// Issue #355 contract: the call-site id must not depend on WHERE the repo
    /// is checked out. An absolute path under one root, the same path under
    /// another root, and the already-relative incremental-cache form must all
    /// hash to the same id.
    #[test]
    fn test_build_site_id_is_repo_root_invariant() {
        let key = OperationKey::http("GET", "/users");
        let from_abs_a = build_site_id("/home/alice/repo/src/api.ts", 12, &key, "/home/alice/repo");
        let from_abs_b = build_site_id("/ci/work/repo/src/api.ts", 12, &key, "/ci/work/repo/");
        let from_rel = build_site_id("src/api.ts", 12, &key, ".");
        let from_dot_rel = build_site_id("./src/api.ts", 12, &key, ".");

        assert_eq!(from_abs_a, from_abs_b);
        assert_eq!(from_abs_a, from_rel);
        assert_eq!(from_abs_a, from_dot_rel);
    }

    /// A path outside the repo root must pass through unchanged, never be
    /// mangled by a partial prefix match (`/repo` vs `/repo-other`).
    #[test]
    fn test_repo_relative_source_path_prefix_safety() {
        assert_eq!(
            repo_relative_source_path("/a/repo-other/src/x.ts", "/a/repo"),
            "/a/repo-other/src/x.ts"
        );
        assert_eq!(
            repo_relative_source_path("/a/repo/src/x.ts", "/a/repo"),
            "src/x.ts"
        );
        assert_eq!(repo_relative_source_path("src/x.ts", "/a/repo"), "src/x.ts");
        assert_eq!(repo_relative_source_path("./src/x.ts", "."), "src/x.ts");
        assert_eq!(
            repo_relative_source_path("/a/repo/src/x.ts", ""),
            "/a/repo/src/x.ts"
        );
    }

    #[test]
    fn test_is_http_method() {
        assert!(is_http_method("get"));
        assert!(is_http_method("POST"));
        assert!(is_http_method("delete"));
        assert!(!is_http_method("unknown"));
        assert!(!is_http_method(".json()"));
    }

    #[test]
    fn test_build_display_name() {
        assert_eq!(
            build_display_name(&OperationKey::http("GET", "/users/:param"), "response"),
            "GET /users/:param → Response"
        );
        assert_eq!(
            build_display_name(&OperationKey::http("POST", "/api/orders"), "request"),
            "POST /api/orders → Request"
        );
        assert_eq!(
            build_display_name(&OperationKey::http("DELETE", "/items/:id"), "Response"),
            "DELETE /items/:id → Response"
        );
    }

    // ---- carrick#780: the placeholder says which it is ---------------------

    /// A bare `unknown` is Carrick saying it has no shape, and says so in the
    /// text. A real type string is a shape and carries no marker — otherwise
    /// every resolved inline alias would read as unresolved.
    #[test]
    fn only_a_bare_unknown_declaration_carries_the_marker() {
        let mut dts = String::new();
        append_alias_declaration(&mut dts, "Endpoint_abc_Response", "unknown");
        append_alias_declaration(&mut dts, "Endpoint_def_Response", "{ id: string }");
        append_alias_declaration(&mut dts, "Endpoint_ghi_Response", "unknown;");

        assert!(
            dts_alias_is_trivially_unknown(&dts, "Endpoint_abc_Response"),
            "the placeholder must be readable as one: {dts}"
        );
        assert!(
            dts_alias_is_trivially_unknown(&dts, "Endpoint_ghi_Response"),
            "a trailing semicolon in the type string is the same statement: {dts}"
        );
        assert!(
            !dts_alias_is_trivially_unknown(&dts, "Endpoint_def_Response"),
            "a resolved shape must never be marked: {dts}"
        );
        assert!(
            dts_defines_alias(&dts, "Endpoint_abc_Response"),
            "the placeholder is still a declaration the bundle can compile"
        );
        assert_eq!(
            dts.matches(MISSING_ALIAS_MARKER).count(),
            2,
            "one marker per placeholder, none elsewhere: {dts}"
        );
    }

    /// A developer's own `type X = unknown` in a real API type is not a
    /// placeholder, in any of the forms it can be written (#244).
    #[test]
    fn an_authored_unknown_is_never_read_as_the_placeholder() {
        for form in [
            "export type OrderView = unknown;\n",
            "type OrderView = unknown;\n",
            "export type OrderView<T> = unknown;\n",
            "export declare type OrderView = unknown;\n",
            "export type OrderView = unknown; // genuinely unknown\n",
        ] {
            assert!(
                !dts_alias_is_trivially_unknown(form, "OrderView"),
                "authored form must not match the placeholder gate: {form:?}"
            );
        }
    }

    /// When an inline alias arrives with the real type, it replaces the
    /// placeholder statement AND the marker: a line carrying a shape must not
    /// go on saying no shape reached the bundle.
    #[test]
    fn replacing_a_placeholder_takes_the_marker_with_it() {
        let mut dts = String::new();
        append_alias_declaration(&mut dts, "OrderView", "unknown");
        append_alias_declaration(&mut dts, "Payment", "unknown");

        assert!(replace_unresolved_alias(
            &mut dts,
            "OrderView",
            "{ id: string }"
        ));

        assert!(
            dts.contains("export type OrderView = { id: string };"),
            "the real type must land: {dts}"
        );
        assert!(
            !dts_alias_is_trivially_unknown(&dts, "OrderView"),
            "the replaced alias must stop reading as unresolved: {dts}"
        );
        assert_eq!(
            dts.matches(MISSING_ALIAS_MARKER).count(),
            1,
            "only the untouched placeholder keeps its marker: {dts}"
        );
        assert!(
            !replace_unresolved_alias(&mut dts, "OrderView", "{ id: number }"),
            "a line that already carries a shape is not a placeholder to replace"
        );
    }

    /// The two writers agree: an alias that never reached the bundle gets the
    /// same statement, in the same form, as one v1 was asked for and could not
    /// answer.
    #[test]
    fn a_missing_alias_gets_the_same_statement_as_an_unresolved_one() {
        let mut asked = String::new();
        append_alias_declaration(&mut asked, "OrderView", "unknown");

        let missing = append_missing_aliases(String::new(), Some(&vec![entry("OrderView")]));

        assert_eq!(asked, missing);
        assert!(dts_alias_is_trivially_unknown(&missing, "OrderView"));
    }

    /// An alias the bundle already declares keeps its declaration.
    #[test]
    fn append_missing_aliases_leaves_a_declared_alias_alone() {
        let dts = "export interface Payment { id: string }\n".to_string();

        let out = append_missing_aliases(dts, Some(&vec![entry("Payment")]));

        assert!(!out.contains("Payment = unknown"), "got: {out}");
        assert!(!dts_alias_is_trivially_unknown(&out, "Payment"));
    }

    fn entry(type_alias: &str) -> crate::cloud_storage::TypeManifestEntry {
        use crate::cloud_storage::{
            ManifestRole, ManifestTypeKind, ManifestTypeState, TypeEvidence, TypeManifestEntry,
        };
        let evidence = TypeEvidence {
            file_path: "lib/api.ts".to_string(),
            span_start: None,
            span_end: None,
            line_number: 5,
            infer_kind: crate::services::type_sidecar::InferKind::CallResult,
            is_explicit: false,
            type_state: ManifestTypeState::Unknown,
        };
        TypeManifestEntry {
            key: OperationKey::http("GET", "/orders/:id"),
            role: ManifestRole::Consumer,
            type_kind: ManifestTypeKind::Response,
            type_alias: type_alias.to_string(),
            file_path: "lib/api.ts".to_string(),
            line_number: 5,
            is_explicit: false,
            type_state: ManifestTypeState::Unknown,
            evidence,
            resolved_definition: None,
            expanded_definition: None,
            primary_type_symbol: None,
            defined_in: None,
            any_provenance: Vec::new(),
            v1_unresolved: false,
        }
    }
}
