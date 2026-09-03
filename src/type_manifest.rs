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
/// `Endpoint_<hash>_<Kind>_Call<id>` aliases. The id is a join key across the
/// manifest, SymbolRequest, and infer-request sides, so every producer of it
/// must call this function with the same `(path, line, key)` triple.
///
/// The path is reduced to its repo-relative form before hashing (issue #355):
/// hashing the absolute path made every `_Call<id>` alias machine-specific,
/// which broke byte-compared goldens across machines and any future
/// output-determinism guarantee. Relativizing INSIDE this function (rather
/// than at call sites) keeps the id identical at every join site regardless of
/// whether the caller holds an absolute full-scan key or a repo-relative
/// incremental key.
pub fn build_call_site_id(
    file_path: &str,
    line_number: u32,
    key: &OperationKey,
    repo_root: &str,
) -> String {
    let relative = repo_relative_source_path(file_path, repo_root);
    let hash_input = format!("{}|{}|{}", relative, line_number, key.canonical());
    format!("{:016x}", fnv1a_hash(&hash_input))
}

pub fn build_manifest_type_alias_with_call_id(
    key: &OperationKey,
    role: ManifestRole,
    type_kind: ManifestTypeKind,
    call_id: Option<&str>,
) -> String {
    let base = build_manifest_type_alias(key, role, type_kind);
    match call_id {
        Some(id) if !id.trim().is_empty() => format!("{}_Call{}", base, id.trim()),
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
    fn test_build_manifest_type_alias_with_call_id() {
        let key = OperationKey::http("GET", "/users");
        let base =
            build_manifest_type_alias(&key, ManifestRole::Consumer, ManifestTypeKind::Response);
        let call_id = build_call_site_id("src/service.ts", 12, &key, ".");
        let with_call = build_manifest_type_alias_with_call_id(
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
    fn test_build_call_site_id_is_repo_root_invariant() {
        let key = OperationKey::http("GET", "/users");
        let from_abs_a =
            build_call_site_id("/home/alice/repo/src/api.ts", 12, &key, "/home/alice/repo");
        let from_abs_b = build_call_site_id("/ci/work/repo/src/api.ts", 12, &key, "/ci/work/repo/");
        let from_rel = build_call_site_id("src/api.ts", 12, &key, ".");
        let from_dot_rel = build_call_site_id("./src/api.ts", 12, &key, ".");

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
}
