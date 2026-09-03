//! How a data call's BASE resolves (carrick#649).
//!
//! A persisted call row states the target it was written against
//! (`${process.env.KNOWLEDGE_URL}/api/lookup`) and the route it keys on
//! (`/api/lookup`). Neither says where the base goes, and that is the deciding
//! fact for "who serves this call": two rows with the same shape — no producer
//! in the workspace, one internal call whose base is a `${…}` expression — have
//! opposite truths when one base is an environment variable declared optional
//! with no default (the producer is outside the workspace) and the other is an
//! injected option defaulting to a loopback URL (something in the workspace
//! serves it).
//!
//! [`CallBaseResolution`] carries that, and only what the AST states. Nothing
//! here classifies a call as internal or external, and nothing here changes a
//! target or a canonical key — it is an additive reading of the base slot the
//! target already resolved to.

use serde::{Deserialize, Serialize};

use crate::env_alias::{EnvAliasMap, EnvFallbackMap, EnvSchemaIndex, is_loopback_origin};

/// What kind of thing supplies a call's base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallBaseKind {
    /// The base reads an environment variable: `process.env.X`,
    /// `import.meta.env.X`, a binding aliasing one, a lone SCREAMING_SNAKE
    /// identifier, or an env-schema access (`env.X`).
    Env,
    /// The base is supplied to this code from somewhere else — a field
    /// (`this.opts.lookupUrl`), a parameter, a config property that reads no
    /// environment variable. The scanner sees the expression and not the value.
    Injected,
    /// There is no base: the call site wrote a bare path (`/api/lookup`), so
    /// the request goes to whatever origin the client already holds.
    Relative,
}

/// The base a call was written against, and — where the scanner can see it —
/// what it resolves to.
///
/// Additive on the persisted row and absent whenever the base is none of the
/// three kinds above (a literal absolute origin, a target with no readable base
/// slot). Every field is what the source states; nothing is inferred from
/// naming or convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallBaseResolution {
    /// The base expression exactly as the resolved target carries it —
    /// `${process.env.KNOWLEDGE_URL}`, `${this.opts.lookupUrl}` — and empty for
    /// a bare relative path, which states no base at all.
    pub written: String,
    /// Which of the three shapes `written` is.
    pub kind: CallBaseKind,
    /// The environment variable the base reads. `None` for every kind but
    /// [`CallBaseKind::Env`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    /// The literal default written next to the environment read
    /// (`?? "http://localhost:3939/api/lookup"`, or a schema
    /// `.default("…")`), verbatim and whole — origin included, because the
    /// origin is the half that says where the request goes when nothing is
    /// configured. `None` when the source writes no literal default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Whether `fallback` names this machine, under the same predicate the
    /// canonical key is computed with
    /// ([`crate::env_alias::whole_url_local_default`]). A loopback default
    /// means the source itself says the unconfigured request stays local.
    pub fallback_is_loopback: bool,
    /// `true` when a validation schema in the scanned files declares this
    /// variable `.optional()` with no default — the source saying it may be
    /// absent at runtime with nothing standing in. `false` when a declaration
    /// exists and states a default. `None` when the scanned files declare
    /// nothing about it, which is not the same as "required": it is the
    /// scanner declining to guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_optional: Option<bool>,
}

/// Read the base of a resolved call target.
///
/// `target` is the target AFTER the base-resolution passes have run, so an
/// env-var base has already been rewritten to its `${process.env.NAME}` form
/// and a literal base has already been spliced in. Returns `None` when the
/// target's base is not one of the three kinds — an absolute literal origin
/// states its host in `target_url` and `host` already, and a target whose base
/// slot cannot be read states nothing to record.
pub fn resolve_call_base(
    target: &str,
    aliases: &EnvAliasMap,
    env_fallbacks: &EnvFallbackMap,
    env_schema: &EnvSchemaIndex,
) -> Option<CallBaseResolution> {
    let trimmed = target.trim().trim_start_matches(['`', '"', '\'']);

    let Some(slot) = leading_interpolation(trimmed) else {
        // No base slot at all. A path is a base-less call; anything else
        // (an absolute URL, a bare identifier) is not this reading's business.
        return trimmed.starts_with('/').then(|| CallBaseResolution {
            written: String::new(),
            kind: CallBaseKind::Relative,
            env_var: None,
            fallback: None,
            fallback_is_loopback: false,
            declared_optional: None,
        });
    };

    let written = format!("${{{slot}}}");
    let Some(env_var) = base_env_var(slot, aliases, env_fallbacks, env_schema) else {
        return Some(CallBaseResolution {
            written,
            kind: CallBaseKind::Injected,
            env_var: None,
            fallback: None,
            fallback_is_loopback: false,
            declared_optional: None,
        });
    };

    // The `??`/`||` literal the binding was declared with, looked up under the
    // spelling at the call site and under the environment variable itself; the
    // schema's `.default("…")` is the same statement made in the other place
    // the source can make it.
    let declaration = env_schema.get(&env_var);
    let fallback = env_fallbacks
        .get(slot)
        .or_else(|| env_fallbacks.get(&env_var))
        .cloned()
        .or_else(|| declaration.and_then(|d| d.default_literal.clone()));

    Some(CallBaseResolution {
        fallback_is_loopback: fallback.as_deref().is_some_and(is_loopback_origin),
        written,
        kind: CallBaseKind::Env,
        env_var: Some(env_var),
        fallback,
        declared_optional: declaration.map(|d| d.optional),
    })
}

/// The text inside a target's LEADING `${…}` — its base slot. `None` when the
/// target does not open with one, which includes a mid-path interpolation
/// (`/things/${id}`): that is a path parameter, not a base.
fn leading_interpolation(target: &str) -> Option<&str> {
    let rest = target.strip_prefix("${")?;
    let end = rest.find('}')?;
    Some(rest[..end].trim())
}

/// The environment variable a base slot reads, or `None` when it reads none —
/// in which case the base is injected, and reported as written.
///
/// Three spellings say it outright and need no corroboration: `process.env.X`,
/// `import.meta.env.X`, and a lone SCREAMING_SNAKE identifier, which is not how
/// anything but an environment variable is named. A binding that aliases an
/// environment read resolves through `aliases`, under its plain or dotted key,
/// exactly as the target rewrite does.
///
/// A DOTTED access whose last segment is SCREAMING_SNAKE (`env.KNOWLEDGE_URL`)
/// is the schema-validated env-object spelling, and it is also what
/// `opts.API_URL` looks like — a value injected into this code, which the
/// scanner sees the expression of and not the value. Naming alone cannot
/// separate them, so this one is read as an environment variable only where the
/// scanned source corroborates it: the name is declared in a validation schema,
/// or it was written with a literal default. Uncorroborated, it stays injected,
/// which is the honest answer and the one #649 exists to stop getting wrong.
/// `this.X` is never it — a field is the injected case by construction.
fn base_env_var(
    slot: &str,
    aliases: &EnvAliasMap,
    env_fallbacks: &EnvFallbackMap,
    env_schema: &EnvSchemaIndex,
) -> Option<String> {
    for prefix in ["process.env.", "import.meta.env."] {
        if let Some(name) = slot.strip_prefix(prefix) {
            return is_plain_identifier(name).then(|| name.to_string());
        }
    }
    if let Some(env_name) = aliases.get(slot) {
        return Some(env_name.clone());
    }
    if !slot.contains('.') {
        return (is_plain_identifier(slot) && crate::env_alias::is_env_var_name(slot))
            .then(|| slot.to_string());
    }
    let mut segments = slot.split('.').map(str::trim);
    if matches!(segments.next(), Some("this" | "self")) {
        return None;
    }
    let last = slot.rsplit('.').next().unwrap_or(slot).trim();
    let corroborated = env_schema.get(last).is_some() || env_fallbacks.contains_key(last);
    (corroborated
        && is_plain_identifier(last)
        && crate::env_alias::is_env_var_name(last)
        && slot
            .split('.')
            .all(|segment| is_plain_identifier(segment.trim())))
    .then(|| last.to_string())
}

/// Whether a segment is a plain JS identifier — no call, no index, no
/// interpolation. Anything else is an expression the scanner is not reading.
fn is_plain_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !text.starts_with(|c: char| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_alias::{EnvAliasExtractor, EnvSchemaIndex, module_env_schema};
    use crate::parser::parse_file;
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };

    /// Parse a source and read back everything `resolve_call_base` needs: the
    /// file's own env bindings and the repo-wide schema index.
    fn bindings(source: &str) -> (EnvAliasMap, EnvFallbackMap, EnvSchemaIndex) {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

        let url_bindings = EnvAliasExtractor::build_bindings(&module);
        let mut schema = EnvSchemaIndex::default();
        schema.merge_module(module_env_schema(&module));
        (url_bindings.aliases, url_bindings.env_fallbacks, schema)
    }

    fn base(source: &str, target: &str) -> Option<CallBaseResolution> {
        let (aliases, fallbacks, schema) = bindings(source);
        resolve_call_base(target, &aliases, &fallbacks, &schema)
    }

    #[test]
    fn an_env_base_with_a_loopback_fallback_states_the_whole_literal() {
        let resolved = base(
            r#"const url = process.env.KNOWLEDGE_URL ?? "http://localhost:3939/api/lookup";"#,
            "${process.env.KNOWLEDGE_URL}/api/lookup",
        )
        .expect("an env base resolves");
        assert_eq!(resolved.written, "${process.env.KNOWLEDGE_URL}");
        assert_eq!(resolved.kind, CallBaseKind::Env);
        assert_eq!(resolved.env_var.as_deref(), Some("KNOWLEDGE_URL"));
        assert_eq!(
            resolved.fallback.as_deref(),
            Some("http://localhost:3939/api/lookup"),
            "the origin is part of the statement, not just the path"
        );
        assert!(resolved.fallback_is_loopback);
        assert_eq!(
            resolved.declared_optional, None,
            "nothing in this file declares the variable"
        );
    }

    #[test]
    fn an_env_base_with_a_third_party_fallback_is_not_loopback() {
        let resolved = base(
            r#"const url = process.env.PAYMENTS_URL ?? "https://api.example.com/v1/charges";"#,
            "${process.env.PAYMENTS_URL}/v1/charges",
        )
        .expect("an env base resolves");
        assert_eq!(
            resolved.fallback.as_deref(),
            Some("https://api.example.com/v1/charges")
        );
        assert!(
            !resolved.fallback_is_loopback,
            "a third-party origin says nothing about this machine"
        );
    }

    #[test]
    fn a_path_less_fallback_is_recorded_where_the_whole_url_map_drops_it() {
        let resolved = base(
            r#"const base = process.env.CATALOG_URL ?? "http://localhost:4001";"#,
            "${process.env.CATALOG_URL}/api/v1/items",
        )
        .expect("an env base resolves");
        assert_eq!(
            resolved.fallback.as_deref(),
            Some("http://localhost:4001"),
            "a base-plus-path call states its default too, even though no \
             route can be read out of it"
        );
        assert!(resolved.fallback_is_loopback);
    }

    #[test]
    fn an_env_var_declared_optional_with_no_default_says_so() {
        let resolved = base(
            r#"export const envSchema = z.object({
                 KNOWLEDGE_URL: z.string().url().optional(),
               });"#,
            "${process.env.KNOWLEDGE_URL}/api/lookup",
        )
        .expect("an env base resolves");
        assert_eq!(resolved.declared_optional, Some(true));
        assert_eq!(
            resolved.fallback, None,
            "an optional variable with no default has no literal to fall back to"
        );
        assert!(!resolved.fallback_is_loopback);
    }

    #[test]
    fn an_env_var_declared_with_a_default_is_not_optional() {
        let resolved = base(
            r#"export const envSchema = z.object({
                 KNOWLEDGE_URL: z.string().default("http://localhost:3939"),
               });"#,
            "${process.env.KNOWLEDGE_URL}/api/lookup",
        )
        .expect("an env base resolves");
        assert_eq!(resolved.declared_optional, Some(false));
        assert_eq!(
            resolved.fallback.as_deref(),
            Some("http://localhost:3939"),
            "a schema default is the same statement a `??` literal makes"
        );
        assert!(resolved.fallback_is_loopback);
    }

    #[test]
    fn an_optional_declaration_that_also_defaults_is_not_optional() {
        let resolved = base(
            r#"export const envSchema = z.object({
                 KNOWLEDGE_URL: z.string().optional().default("http://localhost:3939"),
               });"#,
            "${process.env.KNOWLEDGE_URL}/api/lookup",
        )
        .expect("an env base resolves");
        assert_eq!(
            resolved.declared_optional,
            Some(false),
            "a variable with a default is never absent"
        );
    }

    #[test]
    fn a_schema_that_declares_neither_states_nothing() {
        let resolved = base(
            r#"export const envSchema = z.object({ KNOWLEDGE_URL: z.string().url() });"#,
            "${process.env.KNOWLEDGE_URL}/api/lookup",
        )
        .expect("an env base resolves");
        assert_eq!(
            resolved.declared_optional, None,
            "a required-looking chain that never says `.optional()` or \
             `.default()` is not a statement about optionality"
        );
    }

    #[test]
    fn an_injected_base_is_reported_as_written_and_nothing_more() {
        let resolved = base("", "${this.opts.lookupUrl}/api/lookup").expect("a base slot resolves");
        assert_eq!(resolved.written, "${this.opts.lookupUrl}");
        assert_eq!(resolved.kind, CallBaseKind::Injected);
        assert_eq!(resolved.env_var, None);
        assert_eq!(resolved.fallback, None);
        assert_eq!(resolved.declared_optional, None);
    }

    #[test]
    fn a_bare_relative_path_states_no_base() {
        let resolved = base("", "/api/lookup").expect("a relative path resolves");
        assert_eq!(resolved.written, "");
        assert_eq!(resolved.kind, CallBaseKind::Relative);
        assert_eq!(resolved.env_var, None);
    }

    #[test]
    fn a_local_alias_resolves_to_the_variable_behind_it() {
        let resolved = base(
            r#"const LOOKUP_BASE = process.env.KNOWLEDGE_URL ?? "http://localhost:3939";"#,
            "${LOOKUP_BASE}/api/lookup",
        )
        .expect("an env base resolves");
        assert_eq!(resolved.kind, CallBaseKind::Env);
        assert_eq!(resolved.env_var.as_deref(), Some("KNOWLEDGE_URL"));
        assert_eq!(resolved.written, "${LOOKUP_BASE}");
        assert_eq!(resolved.fallback.as_deref(), Some("http://localhost:3939"));
    }

    #[test]
    fn a_config_property_that_reads_no_environment_is_injected() {
        let resolved = base(
            r#"const config = { lookupUrl: buildUrl() };"#,
            "${config.lookupUrl}/api/lookup",
        )
        .expect("a base slot resolves");
        assert_eq!(resolved.kind, CallBaseKind::Injected);
    }

    #[test]
    fn an_env_schema_access_reads_as_the_variable_it_names() {
        let resolved = base(
            r#"export const envSchema = z.object({
                 KNOWLEDGE_URL: z.string().url().optional(),
               });"#,
            "${env.KNOWLEDGE_URL}/api/lookup",
        )
        .expect("an env base resolves");
        assert_eq!(resolved.kind, CallBaseKind::Env);
        assert_eq!(resolved.env_var.as_deref(), Some("KNOWLEDGE_URL"));
        assert_eq!(resolved.declared_optional, Some(true));
    }

    #[test]
    fn an_uncorroborated_dotted_screaming_snake_stays_injected() {
        for target in ["${opts.API_URL}/api/lookup", "${this.BASE_URL}/api/lookup"] {
            let resolved = base("", target).expect("a base slot resolves");
            assert_eq!(
                resolved.kind,
                CallBaseKind::Injected,
                "{target} is a value handed to this code; nothing in the source \
                 says it reads the environment, and the spelling alone is not \
                 evidence"
            );
            assert_eq!(resolved.env_var, None);
        }
    }

    #[test]
    fn a_field_named_after_a_variable_is_still_injected() {
        let resolved = base(
            r#"export const envSchema = z.object({ BASE_URL: z.string().optional() });"#,
            "${this.BASE_URL}/api/lookup",
        )
        .expect("a base slot resolves");
        assert_eq!(
            resolved.kind,
            CallBaseKind::Injected,
            "a field is the injected case by construction, whatever a schema \
             elsewhere happens to declare under the same name"
        );
    }

    #[test]
    fn a_lone_screaming_snake_identifier_reads_as_a_variable() {
        let resolved = base("", "${KNOWLEDGE_URL}/api/lookup").expect("an env base resolves");
        assert_eq!(resolved.kind, CallBaseKind::Env);
        assert_eq!(resolved.env_var.as_deref(), Some("KNOWLEDGE_URL"));
    }

    #[test]
    fn a_literal_origin_and_a_mid_path_parameter_state_no_base() {
        assert_eq!(base("", "http://localhost:3939/api/lookup"), None);
        assert_eq!(
            base("", "api/lookup/${id}"),
            None,
            "a mid-path interpolation is a path parameter, not a base"
        );
    }

    #[test]
    fn two_files_declaring_one_variable_differently_state_nothing() {
        let mut index = EnvSchemaIndex::default();
        let (_, _, optional) = bindings(
            r#"export const envSchema = z.object({ KNOWLEDGE_URL: z.string().optional() });"#,
        );
        let (_, _, defaulted) = bindings(
            r#"export const envSchema = z.object({ KNOWLEDGE_URL: z.string().default("http://localhost:3939") });"#,
        );
        // Fold the two modules' readings in, in either order.
        index.merge_module(
            optional
                .get("KNOWLEDGE_URL")
                .cloned()
                .map(|d| [("KNOWLEDGE_URL".to_string(), d)].into_iter().collect())
                .unwrap_or_default(),
        );
        index.merge_module(
            defaulted
                .get("KNOWLEDGE_URL")
                .cloned()
                .map(|d| [("KNOWLEDGE_URL".to_string(), d)].into_iter().collect())
                .unwrap_or_default(),
        );
        assert_eq!(
            index.get("KNOWLEDGE_URL"),
            None,
            "two different declarations arbitrate to nothing, not to one of them"
        );
    }
}
