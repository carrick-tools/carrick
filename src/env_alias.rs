//! Env-var alias resolution.
//!
//! A very common real pattern aliases an environment variable through a local
//! const before interpolating it into a request URL:
//!
//! ```ts
//! const ORDERS_BASE = process.env.ORDERS_SERVICE_URL ?? "http://localhost:3001";
//! await fetch(`${ORDERS_BASE}/orders/${orderId}`);
//! ```
//!
//! The file analyzer extracts the call target verbatim as
//! `${ORDERS_BASE}/orders/${orderId}`, so every downstream consumer that keys
//! on the env-var *name* (`Config::is_internal_call`, the cross-repo matcher)
//! sees the local const `ORDERS_BASE` rather than the real env var
//! `ORDERS_SERVICE_URL`. Internal/external classification and cross-repo
//! matching then silently fail.
//!
//! This module builds a per-file map of `local const -> process.env name` for
//! the direct-alias case and rewrites a target URL's leading `${ALIAS}` to
//! `${process.env.NAME}`. That funnels the call back through the existing
//! direct-`process.env` handling in [`crate::url_normalizer`] and
//! [`crate::analyzer`], rather than duplicating env-var parsing.
//!
//! A second, equally common real pattern (#218 cross-file scope) centralizes
//! env reads in a config *object*, often in a separate module:
//!
//! ```ts
//! // config.ts
//! export const config = {
//!   catalogUrl: process.env.CATALOG_URL ?? "http://localhost:4001",
//! };
//! // consumer.ts
//! import { config } from "./config";
//! const client = makeClient(config.catalogUrl);
//! ```
//!
//! The call target then carries `${config.catalogUrl}` as its base. The alias
//! map handles this with *dotted keys* (`config.catalogUrl -> CATALOG_URL`):
//! [`EnvAliasExtractor`] records object-literal properties of local bindings,
//! [`exported_env_aliases`] projects a module's aliases onto its export names,
//! and [`merge_imported_env_aliases`] folds imported modules' exported aliases
//! into the importing file's map under the local import names. The rewrite in
//! [`resolve_target_env_alias`] needs no changes: the text between `${` and
//! `}` is the lookup key whether or not it contains dots.
//!
//! Scope is deliberately structural and tight: only values that read
//! `process.env` *directly* (optionally with a `??`/`||` default), either as a
//! plain binding or one object-literal level deep, are tracked. Anything
//! beyond — reassignment, string concatenation building the base URL, nested
//! config objects (`config.api.url`), `Object.freeze(...)` wrappers, re-export
//! chains (`export * from`), tsconfig path aliases — is intentionally not
//! resolved. See the TODO in [`EnvAliasExtractor`].

use crate::visitor::{ImportedSymbol, SymbolKind};
use std::collections::HashMap;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// Maps a local binding name (e.g. `ORDERS_BASE`) to the `process.env` variable
/// it was initialized from (e.g. `ORDERS_SERVICE_URL`).
pub type EnvAliasMap = HashMap<String, String>;

/// Maps a binding name — and the `process.env` variable behind it — to the
/// whole `??`/`||` fallback URL literal it was declared with
/// (`http://localhost:7100/api/answer` for
/// `process.env.HELPDESK_URL ?? "http://localhost:7100/api/answer"`), for the
/// fallbacks that carry a path.
///
/// The fallback is otherwise discarded: for a base URL the env-var name is all
/// the classifier needs. It matters for exactly one shape, the one this map
/// exists for: a binding that holds a WHOLE URL, passed to a request as-is.
/// The call site then states no path, so the request has no path anywhere
/// except inside that literal, and without it the call is dropped for having
/// no route shape.
///
/// The origin is kept alongside the path because the source states it too, and
/// for a LOOPBACK default it is the only thing that says where the request goes
/// when nothing is configured. [`whole_url_local_default`] reads it back.
pub type WholeUrlFallbackMap = HashMap<String, String>;

/// Maps a binding name — and the `process.env` variable behind it — to the
/// WHOLE `??`/`||` string literal it was declared with, whatever that literal
/// says.
///
/// [`WholeUrlFallbackMap`] above is gated on the fallback being an absolute URL
/// that carries a path, because that gate is what makes a route safe to READ
/// OUT of the fallback. This map resolves nothing and states no route: it is
/// the literal the source wrote next to the env read, kept verbatim so a call's
/// persisted base can say what the request falls back to when the environment
/// supplies nothing. `?? "https://api.example.com/v1/ask"` is as much a fact
/// about the call as `?? "http://localhost:3939/api/lookup"` is; only the
/// second is safe to key on, and both are worth recording (carrick#649).
///
/// Deliberately a separate, additive map: widening the whole-URL map to hold
/// path-less and third-party literals would change which targets
/// [`resolve_whole_url_target`] rewrites.
pub type EnvFallbackMap = HashMap<String, String>;

/// What a validation schema in the scanned source declares about one
/// environment variable.
///
/// Read structurally, from an object-literal property whose key is a
/// SCREAMING_SNAKE name and whose value is a builder chain — the shape an env
/// block has in every schema library, with no library name anywhere in the
/// read. The property key has to equal an environment variable the call's base
/// named independently, so a non-env schema field can only collide by being
/// literally named after one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSchemaDeclaration {
    /// The chain calls `.optional()` and states no default — so the source
    /// says outright that this variable may be absent at runtime with nothing
    /// standing in for it.
    ///
    /// `false` whenever a `.default(…)` is present, whether or not
    /// `.optional()` is too: a variable with a default is never absent.
    pub optional: bool,
    /// The string literal a `.default("…")` in the chain states, when it
    /// states one. `None` for a non-literal default (`.default(computePort())`)
    /// as well as for no default at all — see `optional` for which it was.
    pub default_literal: Option<String>,
}

/// Environment-variable name -> what the scanned source declares about it.
///
/// Repo-wide rather than per-file, because an environment variable IS
/// process-global: the name is the same fact wherever it is read, and the file
/// that declares the schema is usually not the file that makes the call. Two
/// DIFFERENT declarations of one name state nothing decidable, so
/// [`merge_env_schema`] drops such a name rather than picking one.
pub type EnvSchemaMap = HashMap<String, EnvSchemaDeclaration>;

/// Maps a module-level binding name to the absolute URL literal it was declared
/// with (`https://api.example.com` for `const BASE = "https://api.example.com"`).
///
/// A base declared once as a plain literal and interpolated at the call
/// (`fetch(`${BASE}/api/v1/whoami`)`) is the only base shape the scanner left
/// unresolved: an env-var base resolves through [`EnvAliasMap`] and a member
/// expression (`${this.baseUrl}/…`) is carried as-is, but the identifier kept
/// its `${BASE}` prefix, so the canonical key never reduced to the route path
/// and the call matched nothing (carrick#627).
pub type LiteralBaseMap = HashMap<String, String>;

/// The URL-shaped bindings one module declares, all read off the same walk.
pub struct UrlBindings {
    pub aliases: EnvAliasMap,
    pub whole_url_fallbacks: WholeUrlFallbackMap,
    /// Every `??`/`||` string-literal default, gated on nothing.
    pub env_fallbacks: EnvFallbackMap,
    pub literal_bases: LiteralBaseMap,
}

/// Visitor that collects `const/let/var X = process.env.NAME [?? default]`
/// bindings — and object-literal config properties
/// (`const config = { url: process.env.NAME }`, recorded under the dotted key
/// `config.url`) — into an [`EnvAliasMap`].
///
/// TODO(#218 follow-up): only direct `process.env` reads (plain or one
/// object-literal level deep) are tracked. We do not follow reassignments,
/// concatenated/templated bases (`const b = process.env.X + "/v1"`), nested
/// objects, or `Object.freeze(...)` wrappers. Those need real data-flow
/// analysis and are out of scope for the deterministic structural fix.
#[derive(Default)]
pub struct EnvAliasExtractor {
    pub aliases: EnvAliasMap,
    pub whole_url_fallbacks: WholeUrlFallbackMap,
    pub env_fallbacks: EnvFallbackMap,
}

impl EnvAliasExtractor {
    /// Build the alias map for a parsed module.
    pub fn build(module: &Module) -> EnvAliasMap {
        Self::build_bindings(module).aliases
    }

    /// Build every URL-shaped binding the module declares.
    pub fn build_bindings(module: &Module) -> UrlBindings {
        let mut extractor = EnvAliasExtractor::default();
        module.visit_with(&mut extractor);
        UrlBindings {
            aliases: extractor.aliases,
            whole_url_fallbacks: extractor.whole_url_fallbacks,
            env_fallbacks: extractor.env_fallbacks,
            literal_bases: module_literal_bases(module),
        }
    }
}

/// The absolute URL literals a module declares at its TOP LEVEL, exported or
/// not (carrick#627).
///
/// Module level, and not one statement deeper, because the map is keyed on the
/// binding name alone and a name is only unambiguous where there is one of it.
/// A URL literal held in a function-local `const url = "https://…"` is common
/// enough that two functions in one file will each have their own, and a flat
/// map would resolve one function's call against the other's host. A
/// module-level const is the base every call in the file shares, which is the
/// shape this reads and the only one where the name settles the value.
fn module_literal_bases(module: &Module) -> LiteralBaseMap {
    let mut bases = LiteralBaseMap::new();
    for item in &module.body {
        let var_decl = match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var_decl))) => var_decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                Decl::Var(var_decl) => var_decl,
                _ => continue,
            },
            _ => continue,
        };
        for decl in &var_decl.decls {
            let Pat::Ident(binding) = &decl.name else {
                continue;
            };
            let Some(init) = &decl.init else {
                continue;
            };
            if let Some(base) = absolute_url_literal(init) {
                bases.insert(binding.id.sym.to_string(), base);
            }
        }
    }
    bases
}

impl Visit for EnvAliasExtractor {
    fn visit_var_decl(&mut self, var_decl: &VarDecl) {
        for decl in &var_decl.decls {
            // Only simple identifier bindings: `const X = ...`. Destructuring
            // (`const { X } = process.env`) is a different, rarer pattern.
            let Pat::Ident(binding) = &decl.name else {
                continue;
            };
            let Some(init) = &decl.init else {
                continue;
            };

            if let Some(env_name) = process_env_name(init) {
                // `const url = process.env.X ?? "http://host:port/api/answer"`.
                // The whole URL comes from the env var, so the binding is not
                // a base a path is appended to and the call site interpolates
                // nothing after it. The only path the source states for this
                // URL is the fallback's, so record it against both the local
                // name and the env-var name; `resolve_whole_url_target` reads
                // it back for a target that states no path at all.
                if let Some(fallback) = fallback_url_literal(init) {
                    self.whole_url_fallbacks
                        .insert(binding.id.sym.to_string(), fallback.clone());
                    self.whole_url_fallbacks.insert(env_name.clone(), fallback);
                }
                // The same `??`/`||` literal, ungated (carrick#649). Recorded
                // for what it says about the call's base rather than to read a
                // route out of, so a path-less origin and a third-party one are
                // kept exactly as a loopback route is.
                if let Some(fallback) = fallback_literal(init) {
                    self.env_fallbacks
                        .insert(binding.id.sym.to_string(), fallback.clone());
                    self.env_fallbacks.insert(env_name.clone(), fallback);
                }
                // SWC resolver gives each binding a unique SyntaxContext, but the
                // call target the LLM emits is just the bare symbol text. Key on
                // the symbol so `${ORDERS_BASE}` resolves. A name shadowed in a
                // nested scope would collide here, but that is vanishingly rare
                // for a base-URL const and far better than not resolving at all.
                self.aliases.insert(binding.id.sym.to_string(), env_name);
            } else if let Expr::Object(obj) = unwrap_transparent(init) {
                // Config-object pattern (#218 cross-file scope): record each
                // env-reading property under the dotted key `binding.prop`, so
                // a call target of `${config.catalogUrl}/...` resolves through
                // the same map lookup as a plain alias.
                for (prop, env_name) in object_env_props(obj, &self.aliases) {
                    self.aliases
                        .insert(format!("{}.{}", binding.id.sym, prop), env_name);
                }
            }
        }

        var_decl.visit_children_with(self);
    }
}

/// Collect `(property_name, env_var_name)` pairs from an object literal's
/// key-value properties whose values read `process.env` directly — or reference
/// an already-collected local alias (`{ catalogUrl: CATALOG_BASE }` /
/// shorthand `{ catalogUrl }` where `const CATALOG_BASE = process.env.X`
/// appeared earlier in the file). Spreads, methods, computed keys, and nested
/// objects are skipped: they need data-flow analysis, not a structural read.
fn object_env_props(obj: &ObjectLit, known_aliases: &EnvAliasMap) -> Vec<(String, String)> {
    let mut props = Vec::new();
    for prop in &obj.props {
        let PropOrSpread::Prop(prop) = prop else {
            continue;
        };
        match &**prop {
            Prop::KeyValue(kv) => {
                let name = match &kv.key {
                    PropName::Ident(ident) => ident.sym.to_string(),
                    PropName::Str(s) => s.value.to_string(),
                    _ => continue,
                };
                let env_name = process_env_name(&kv.value).or_else(|| {
                    // A property referencing a local alias binding.
                    match unwrap_transparent(&kv.value) {
                        Expr::Ident(ident) => known_aliases.get(ident.sym.as_ref()).cloned(),
                        _ => None,
                    }
                });
                if let Some(env_name) = env_name {
                    props.push((name, env_name));
                }
            }
            // `{ catalogUrl }` shorthand for a local alias binding.
            Prop::Shorthand(ident) => {
                if let Some(env_name) = known_aliases.get(ident.sym.as_ref()) {
                    props.push((ident.sym.to_string(), env_name.clone()));
                }
            }
            _ => {}
        }
    }
    props
}

/// Strip expression wrappers that do not change the runtime value:
/// parentheses, `as` / `satisfies` / `as const` assertions, and non-null `!`.
fn unwrap_transparent(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(e) => unwrap_transparent(&e.expr),
        Expr::TsAs(e) => unwrap_transparent(&e.expr),
        Expr::TsConstAssertion(e) => unwrap_transparent(&e.expr),
        Expr::TsSatisfies(e) => unwrap_transparent(&e.expr),
        Expr::TsNonNull(e) => unwrap_transparent(&e.expr),
        _ => expr,
    }
}

/// The env aliases a module makes visible to its importers, keyed by *export*
/// name: `config.catalogUrl` for `export const config = { catalogUrl:
/// process.env.CATALOG_URL }`, `CATALOG_BASE` for `export const CATALOG_BASE =
/// process.env.CATALOG_URL`, and `default` / `default.prop` for the default
/// export. `export { a as b }` renames are followed; re-exports
/// (`export * from`, `export { x } from "./y"`) are not — they would need
/// recursive module resolution (documented limitation).
pub fn exported_env_aliases(module: &Module) -> EnvAliasMap {
    let locals = EnvAliasExtractor::build(module);

    // (exported name, local name) pairs for every export that could carry an
    // env alias.
    let mut exports: Vec<(String, String)> = Vec::new();
    let mut out = EnvAliasMap::new();

    for item in &module.body {
        let ModuleItem::ModuleDecl(decl) = item else {
            continue;
        };
        match decl {
            // `export const config = {...}` / `export const BASE = process.env.X`
            ModuleDecl::ExportDecl(export_decl) => {
                if let Decl::Var(var_decl) = &export_decl.decl {
                    for d in &var_decl.decls {
                        if let Pat::Ident(binding) = &d.name {
                            let name = binding.id.sym.to_string();
                            exports.push((name.clone(), name));
                        }
                    }
                }
            }
            // `export { config }` / `export { config as settings }` — local
            // bindings only; `export { x } from "./y"` re-exports carry no
            // local binding to resolve.
            ModuleDecl::ExportNamed(named) if named.src.is_none() => {
                for spec in &named.specifiers {
                    let ExportSpecifier::Named(named_spec) = spec else {
                        continue;
                    };
                    let ModuleExportName::Ident(orig) = &named_spec.orig else {
                        continue;
                    };
                    let exported = match &named_spec.exported {
                        Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                        Some(ModuleExportName::Str(s)) => s.value.to_string(),
                        None => orig.sym.to_string(),
                    };
                    exports.push((exported, orig.sym.to_string()));
                }
            }
            // `export default config` / `export default {...}`
            ModuleDecl::ExportDefaultExpr(default_expr) => {
                match unwrap_transparent(&default_expr.expr) {
                    Expr::Ident(ident) => {
                        exports.push(("default".to_string(), ident.sym.to_string()));
                    }
                    Expr::Object(obj) => {
                        for (prop, env_name) in object_env_props(obj, &locals) {
                            out.insert(format!("default.{}", prop), env_name);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    for (exported, local) in exports {
        // Plain alias exported under this name.
        if let Some(env_name) = locals.get(&local) {
            out.insert(exported.clone(), env_name.clone());
        }
        // Config-object properties exported under this name.
        let prefix = format!("{}.", local);
        for (key, env_name) in &locals {
            if let Some(suffix) = key.strip_prefix(&prefix) {
                out.insert(format!("{}.{}", exported, suffix), env_name.clone());
            }
        }
    }

    out
}

/// Fold imported modules' exported env aliases into an importing file's alias
/// map, keyed under the file's *local* import names so call-target lookups
/// resolve directly:
///
/// - named import `import { config } from "./config"` (renames included) maps
///   the source module's `config` / `config.prop` keys to the local name;
/// - default import maps the source's `default` / `default.prop` keys;
/// - namespace import `import * as cfg` maps every exported key under `cfg.`.
///
/// `resolve_module` maps an import specifier to that module's
/// [`exported_env_aliases`] (or `None` when the specifier does not resolve to
/// a parseable same-repo file). Locally-defined aliases always win: imports
/// only fill vacant keys.
pub fn merge_imported_env_aliases<F>(
    aliases: &mut EnvAliasMap,
    imported_symbols: &HashMap<String, ImportedSymbol>,
    mut resolve_module: F,
) where
    F: FnMut(&str) -> Option<EnvAliasMap>,
{
    for (local_name, symbol) in imported_symbols {
        let Some(exports) = resolve_module(&symbol.source) else {
            continue;
        };
        if exports.is_empty() {
            continue;
        }
        match symbol.kind {
            SymbolKind::Named | SymbolKind::Default => {
                let exported_name = match symbol.kind {
                    SymbolKind::Default => "default",
                    _ => symbol.imported_name.as_str(),
                };
                if let Some(env_name) = exports.get(exported_name) {
                    aliases
                        .entry(local_name.clone())
                        .or_insert_with(|| env_name.clone());
                }
                let prefix = format!("{}.", exported_name);
                for (key, env_name) in &exports {
                    if let Some(suffix) = key.strip_prefix(&prefix) {
                        aliases
                            .entry(format!("{}.{}", local_name, suffix))
                            .or_insert_with(|| env_name.clone());
                    }
                }
            }
            SymbolKind::Namespace => {
                for (key, env_name) in &exports {
                    aliases
                        .entry(format!("{}.{}", local_name, key))
                        .or_insert_with(|| env_name.clone());
                }
            }
        }
    }
}

/// If `expr` reads a single `process.env` variable (optionally with a `??`/`||`
/// default), return that variable's name.
///
/// Handles:
/// - `process.env.NAME`
/// - `process.env["NAME"]`
/// - `process.env.NAME ?? <default>` / `process.env.NAME || <default>`
///
/// Transparent wrappers (parens, `!`, `as`, `as const`, `satisfies`) are
/// stripped via [`unwrap_transparent`] before matching, so every caller —
/// direct-alias bindings AND config-object property values — recognizes a
/// wrapped env read.
pub(crate) fn process_env_name(expr: &Expr) -> Option<String> {
    match unwrap_transparent(expr) {
        Expr::Member(member) => process_env_member_name(member),
        // `process.env.NAME ?? "default"` / `... || "default"`: the env read is
        // the left operand. The default literal is discarded — the env-var name
        // is all the classifier needs.
        Expr::Bin(bin) if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) => {
            process_env_name(&bin.left)
        }
        _ => None,
    }
}

/// If `member` is `process.env.NAME` or `process.env["NAME"]`, return `NAME`.
fn process_env_member_name(member: &MemberExpr) -> Option<String> {
    // The object must be exactly `process.env`.
    let Expr::Member(obj) = &*member.obj else {
        return None;
    };
    if !is_ident(&obj.obj, "process") || !is_ident_prop(&obj.prop, "env") {
        return None;
    }

    match &member.prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
            _ => None,
        },
        MemberProp::PrivateName(_) => None,
    }
}

fn is_ident(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::Ident(ident) if ident.sym.as_ref() == name)
}

fn is_ident_prop(prop: &MemberProp, name: &str) -> bool {
    matches!(prop, MemberProp::Ident(ident) if ident.sym.as_ref() == name)
}

/// Rewrite a leading `${ALIAS}` in a call target to `${process.env.NAME}` when
/// `ALIAS` is a known env-var alias, so the existing direct-`process.env`
/// handling resolves the real env-var name.
///
/// Only the *leading* interpolation is considered: that is the base-URL slot.
/// A mid-path `${id}` is a path parameter, never an env alias, so it is left
/// untouched. Returns `None` when nothing was rewritten.
pub fn resolve_target_env_alias(target: &str, aliases: &EnvAliasMap) -> Option<String> {
    if aliases.is_empty() {
        return None;
    }

    // The analyzer/normalizer trim wrapper backticks/quotes themselves, but the
    // alias sits at the very front, so peek past any leading wrapper char.
    let trimmed = target.trim_start_matches(['`', '"', '\'']);
    let rest = trimmed.strip_prefix("${")?;
    let end = rest.find('}')?;
    let alias = &rest[..end];

    let env_name = aliases.get(alias)?;

    // Splice `process.env.NAME` in for the bare alias, preserving everything the
    // wrapper-trim skipped and the rest of the path verbatim.
    let prefix_len = target.len() - trimmed.len();
    let after_brace = &rest[end + 1..];
    Some(format!(
        "{}${{process.env.{}}}{}",
        &target[..prefix_len],
        env_name,
        after_brace
    ))
}

/// Rewrite a leading `${BASE}` in a call target to the absolute URL literal
/// `BASE` was declared with, so the target states the same request the
/// absolute-host form does (carrick#627).
///
/// `const BASE = "https://api.example.com"` followed by
/// `fetch(`${BASE}/api/v1/whoami`)` is one request to `/api/v1/whoami` on a
/// host the file names outright. Left unresolved, the `${BASE}` prefix stays in
/// the target, the canonical key never reduces to the route path, and the call
/// matches no producer and reaches no query surface.
///
/// Same walk as [`resolve_target_env_alias`], and the same restriction to the
/// LEADING interpolation: that is the base-URL slot, where a mid-path `${id}`
/// is a path parameter. Only a base declared in the same file is read; one
/// imported from another module is not, for the same reason re-export chains
/// are not followed. Returns `None` when nothing was rewritten.
pub fn resolve_target_literal_base(target: &str, bases: &LiteralBaseMap) -> Option<String> {
    if bases.is_empty() {
        return None;
    }

    // The analyzer/normalizer trim wrapper backticks/quotes themselves, but the
    // base sits at the very front, so peek past any leading wrapper char.
    let trimmed = target.trim_start_matches(['`', '"', '\'']);
    let rest = trimmed.strip_prefix("${")?;
    let end = rest.find('}')?;
    let base = bases.get(&rest[..end])?;

    let prefix_len = target.len() - trimmed.len();
    let after_brace = &rest[end + 1..];
    // One slash at the join. A base written with a trailing slash and a path
    // written with a leading one is the same URL either way; anything else is
    // concatenated exactly as the source concatenates it.
    let base = if after_brace.starts_with('/') {
        base.trim_end_matches('/')
    } else {
        base.as_str()
    };
    Some(format!("{}{}{}", &target[..prefix_len], base, after_brace))
}

/// An env read's fallback URL literal, when the fallback is an absolute URL
/// that carries a path.
///
/// `process.env.X ?? "http://localhost:7100/api/answer"` yields the whole
/// literal. A fallback with no path (`"http://localhost:7100"`), a relative
/// one, or a non-literal yields `None`: there is nothing to state.
fn fallback_url_literal(expr: &Expr) -> Option<String> {
    let Expr::Bin(bin) = unwrap_transparent(expr) else {
        return None;
    };
    if !matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) {
        return None;
    }
    let literal = string_literal(&bin.right)?;
    (!url_literal_path(&literal)?.is_empty()).then_some(literal)
}

/// An env read's `??`/`||` fallback string literal, whatever it says
/// (carrick#649).
///
/// [`fallback_url_literal`] above gates on the literal being an absolute URL
/// with a path, because a route is read out of it. Nothing is read out of this
/// one: it is reported verbatim as what the source states the request falls
/// back to. A non-literal default (a template, a function call, another
/// binding) still yields `None` — there is no written literal to report.
fn fallback_literal(expr: &Expr) -> Option<String> {
    let Expr::Bin(bin) = unwrap_transparent(expr) else {
        return None;
    };
    matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr)
        .then(|| string_literal(&bin.right))
        .flatten()
}

/// The path of an absolute `http(s)` URL literal, empty when it states none.
/// `None` when the literal is not an absolute URL at all.
fn url_literal_path(literal: &str) -> Option<&str> {
    let after_scheme = literal
        .strip_prefix("http://")
        .or_else(|| literal.strip_prefix("https://"))?;
    let Some(slash) = after_scheme.find('/') else {
        return Some("");
    };
    let path = &after_scheme[slash..];
    Some(if path.len() > 1 { path } else { "" })
}

/// Whether an absolute URL literal's host is a LOOPBACK address — this machine,
/// under any of the spellings a developer writes.
pub fn is_loopback_origin(literal: &str) -> bool {
    let Some(after_scheme) = literal
        .strip_prefix("http://")
        .or_else(|| literal.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Userinfo before the host, port after it. An IPv6 host is bracketed, so
    // its own colons are inside the brackets and the port split is safe once
    // the brackets are stripped.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = match host_port.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => host_port.split(':').next().unwrap_or(host_port),
    };
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1")
}

/// The value of an initializer that is an absolute URL string literal.
///
/// Narrow on purpose. A scheme and a host is what makes the value unambiguously
/// an origin, so splicing it into the base slot of a target says exactly what
/// the source says. Any other literal (`"/api/v1"`, a flag, a name) would be
/// spliced into the same slot on the strength of the binding's position alone,
/// which is a guess, and an interpolated literal states no fixed origin at all.
fn absolute_url_literal(expr: &Expr) -> Option<String> {
    let literal = string_literal(expr)?;
    let after_scheme = literal
        .strip_prefix("http://")
        .or_else(|| literal.strip_prefix("https://"))?;
    // A scheme with no host behind it is not an origin.
    (!after_scheme.trim().is_empty() && !after_scheme.starts_with('/')).then_some(literal)
}

/// The value of a plain string literal, ignoring template literals: a fallback
/// URL that interpolates something is not a literal statement of a path.
fn string_literal(expr: &Expr) -> Option<String> {
    match unwrap_transparent(expr) {
        Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
        _ => None,
    }
}

/// Rewrite a call target that is nothing but an env-var-backed binding into
/// that env var plus the path its fallback literal states.
///
/// `fetch(url, …)` where `const url = process.env.HELPDESK_URL ?? "http://localhost:7100/api/answer"`
/// gives extraction nothing to write down but the binding. A bare identifier
/// is not route-shaped, so the call is dropped before it becomes a row of any
/// kind: not a matched edge, not an unmatched call, not an egress candidate.
/// The request is real and its path is written down in the file, just inside
/// the fallback rather than at the call site.
///
/// Fires only when the target states no path of its own — `url`, `${url}`, or
/// `${process.env.HELPDESK_URL}` once the inline-fallback fold has run — so
/// a base with a path behind it is never touched. What it asserts is the
/// source's own statement about that URL, and no more: the env var supplies
/// the origin, the fallback supplies the path.
pub fn resolve_whole_url_target(
    target: &str,
    aliases: &EnvAliasMap,
    fallbacks: &WholeUrlFallbackMap,
) -> Option<String> {
    let (env_name, fallback) = whole_url_binding(target, aliases, fallbacks)?;
    let path = url_literal_path(fallback)?;
    Some(format!("${{process.env.{env_name}}}{path}"))
}

/// The concrete URL a whole-URL env-var call falls back to, when that fallback
/// is on LOOPBACK.
///
/// `resolve_whole_url_target` above states the call's target as the env var
/// plus the fallback's path, which is what the source says about the deployed
/// request. It leaves the KEY unmatchable: an undeclared env-var base is kept
/// verbatim (see `UrlNormalizer::consumer_call_path`), so the origin survives
/// into the canonical path as a leading segment and the call cannot be found
/// under the path it actually requests.
///
/// Keeping it verbatim is right where the base is opaque — "there is no
/// concrete origin to strip" is the whole reasoning — but here the source
/// states one, and a loopback default is this machine, not a third party. That
/// is the same structural fact a literal absolute origin carries, so the call
/// keys the way `fetch("http://localhost:3939/api/ask")` already does. The
/// gate is the loopback host and nothing else: `?? "https://api.stripe.com/v1"`
/// states a third-party origin and stays verbatim, undeclared as ever.
pub fn whole_url_local_default(
    target: &str,
    aliases: &EnvAliasMap,
    fallbacks: &WholeUrlFallbackMap,
) -> Option<String> {
    let (_, fallback) = whole_url_binding(target, aliases, fallbacks)?;
    is_loopback_origin(fallback).then(|| fallback.to_string())
}

/// What every validation schema in the scanned repo declares about the
/// environment (carrick#649), folded one module at a time.
///
/// A name that two modules declare DIFFERENTLY is dropped, not arbitrated: the
/// source states two things and the scanner has no basis for picking one, so
/// the honest answer downstream is "no declaration found". Dropping is also
/// what makes the index independent of the order files are walked in.
#[derive(Default, Debug)]
pub struct EnvSchemaIndex {
    declarations: EnvSchemaMap,
    /// Names seen with two different declarations. Kept so a third module
    /// restating one of the two cannot resurrect it.
    ambiguous: std::collections::HashSet<String>,
}

impl EnvSchemaIndex {
    /// Fold one module's declarations in.
    pub fn merge_module(&mut self, module_declarations: EnvSchemaMap) {
        for (name, declaration) in module_declarations {
            if self.ambiguous.contains(&name) {
                continue;
            }
            match self.declarations.get(&name) {
                Some(existing) if *existing != declaration => {
                    self.declarations.remove(&name);
                    self.ambiguous.insert(name);
                }
                Some(_) => {}
                None => {
                    self.declarations.insert(name, declaration);
                }
            }
        }
    }

    /// What the source declares about one environment variable, or `None` when
    /// nothing in the scanned files declares it (or two files disagree).
    pub fn get(&self, env_var: &str) -> Option<&EnvSchemaDeclaration> {
        self.declarations.get(env_var)
    }
}

/// The environment-variable declarations one module's validation schemas state.
///
/// Structural and library-agnostic: any object-literal property whose key is a
/// SCREAMING_SNAKE name and whose value is a builder chain that calls
/// `.optional()` or `.default(…)`. A chain stating NEITHER is not recorded —
/// there is nothing about optionality to report, and inventing "required" from
/// a chain that never said so would be a guess.
pub fn module_env_schema(module: &Module) -> EnvSchemaMap {
    let mut extractor = EnvSchemaExtractor::default();
    module.visit_with(&mut extractor);
    extractor.declarations
}

#[derive(Default)]
struct EnvSchemaExtractor {
    declarations: EnvSchemaMap,
}

impl Visit for EnvSchemaExtractor {
    fn visit_object_lit(&mut self, obj: &ObjectLit) {
        for prop in &obj.props {
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            let Prop::KeyValue(kv) = &**prop else {
                continue;
            };
            let name = match &kv.key {
                PropName::Ident(ident) => ident.sym.to_string(),
                PropName::Str(s) => s.value.to_string(),
                _ => continue,
            };
            if !is_env_var_name(&name) {
                continue;
            }
            if let Some(declaration) = schema_chain_declaration(&kv.value) {
                // First declaration in the module wins; the repo-wide fold
                // above is what resolves a name declared twice.
                self.declarations.entry(name).or_insert(declaration);
            }
        }
        obj.visit_children_with(self);
    }
}

/// Whether a name is spelled the way environment variables are: SCREAMING_SNAKE
/// with at least one letter (`KNOWLEDGE_URL`, `PORT`). This is the whole
/// discriminator between an env block and any other object literal, and it is
/// only ever consulted for a name the call's base already produced.
pub(crate) fn is_env_var_name(name: &str) -> bool {
    name.len() >= 2
        && name.chars().any(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Read `.optional()` and `.default(…)` off a builder chain, walking from the
/// outermost call down to whatever the chain starts at.
///
/// Returns `None` when the chain states neither — see [`module_env_schema`].
fn schema_chain_declaration(expr: &Expr) -> Option<EnvSchemaDeclaration> {
    let mut optional = false;
    let mut has_default = false;
    let mut default_literal: Option<String> = None;
    let mut current = unwrap_transparent(expr);

    loop {
        match current {
            Expr::Call(call) => {
                let Callee::Expr(callee) = &call.callee else {
                    break;
                };
                let callee = unwrap_transparent(callee);
                let Expr::Member(member) = callee else {
                    current = callee;
                    continue;
                };
                if let MemberProp::Ident(method) = &member.prop {
                    match method.sym.as_ref() {
                        "optional" => optional = true,
                        "default" => {
                            has_default = true;
                            // The outermost `.default` wins, which is the one
                            // whose value the chain actually yields.
                            if default_literal.is_none() {
                                default_literal = call
                                    .args
                                    .first()
                                    .filter(|arg| arg.spread.is_none())
                                    .and_then(|arg| string_literal(&arg.expr));
                            }
                        }
                        _ => {}
                    }
                }
                current = unwrap_transparent(&member.obj);
            }
            Expr::Member(member) => current = unwrap_transparent(&member.obj),
            _ => break,
        }
    }

    (optional || has_default).then_some(EnvSchemaDeclaration {
        optional: optional && !has_default,
        default_literal,
    })
}

/// The `(env var name, fallback URL literal)` behind a target that is nothing
/// but an env-var-backed binding.
fn whole_url_binding<'a>(
    target: &str,
    aliases: &EnvAliasMap,
    fallbacks: &'a WholeUrlFallbackMap,
) -> Option<(String, &'a str)> {
    if fallbacks.is_empty() {
        return None;
    }
    let trimmed = target.trim().trim_matches(['`', '"', '\'']).trim();
    // `${NAME}` and a bare `NAME` are the same statement here.
    let name = trimmed
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(trimmed)
        .trim();
    let env_name = match name.strip_prefix("process.env.") {
        Some(env_name) => env_name,
        None => aliases.get(name).map(String::as_str)?,
    };
    let fallback = fallbacks.get(name).or_else(|| fallbacks.get(env_name))?;
    Some((env_name.to_string(), fallback.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };

    fn build_map(source: &str) -> EnvAliasMap {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

        EnvAliasExtractor::build(&module)
    }

    fn build_literal_bases(source: &str) -> LiteralBaseMap {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

        EnvAliasExtractor::build_bindings(&module).literal_bases
    }

    #[test]
    fn a_literal_base_resolves_to_the_url_it_was_declared_with() {
        let bases = build_literal_bases(r#"const BASE = "https://api.example.com";"#);
        assert_eq!(
            resolve_target_literal_base("`${BASE}/api/v1/whoami`", &bases).as_deref(),
            Some("`https://api.example.com/api/v1/whoami`"),
            "the base slot states an origin, and the rest of the target is verbatim"
        );
    }

    #[test]
    fn a_literal_base_joins_on_one_slash() {
        let bases = build_literal_bases(r#"const BASE = "http://localhost:30303/";"#);
        assert_eq!(
            resolve_target_literal_base("${BASE}/api/v1/whoami", &bases).as_deref(),
            Some("http://localhost:30303/api/v1/whoami")
        );
        assert_eq!(
            resolve_target_literal_base("${BASE}api/v1/whoami", &bases).as_deref(),
            Some("http://localhost:30303/api/v1/whoami"),
            "a path with no leading slash is concatenated as the source concatenates it"
        );
    }

    #[test]
    fn only_an_absolute_url_literal_is_read_as_a_base() {
        let bases = build_literal_bases(
            r#"const PREFIX = "/api/v1";
               const NAME = "orders-service";
               const SCHEME_ONLY = "https://";
               const TEMPLATED = `https://${host}`;"#,
        );
        assert!(
            bases.is_empty(),
            "a path, a name, a scheme with no host, and an interpolated literal \
             all state no origin: {bases:?}"
        );
    }

    #[test]
    fn a_function_local_url_literal_is_not_read_as_a_base() {
        let bases = build_literal_bases(
            r#"export function first() { const url = "https://one.example.com"; return url; }
               export function second() { const url = "https://two.example.com"; return url; }"#,
        );
        assert!(
            bases.is_empty(),
            "the map is keyed on the name alone, and a function-local name is \
             not unique in a file: {bases:?}"
        );
    }

    #[test]
    fn an_exported_module_level_literal_is_read_as_a_base() {
        let bases = build_literal_bases(r#"export const BASE = "https://api.example.com";"#);
        assert_eq!(
            bases.get("BASE").map(String::as_str),
            Some("https://api.example.com")
        );
    }

    #[test]
    fn a_literal_base_rule_leaves_a_mid_path_interpolation_alone() {
        let bases = build_literal_bases(r#"const BASE = "https://api.example.com";"#);
        assert_eq!(
            resolve_target_literal_base("/api/v1/things/${BASE}", &bases),
            None,
            "only the leading interpolation is the base slot"
        );
    }

    fn build_both(source: &str) -> (EnvAliasMap, WholeUrlFallbackMap) {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

        let bindings = EnvAliasExtractor::build_bindings(&module);
        (bindings.aliases, bindings.whole_url_fallbacks)
    }

    #[test]
    fn whole_url_target_takes_its_path_from_the_fallback_literal() {
        let (aliases, paths) = build_both(
            r#"const url = process.env.HELPDESK_URL ?? "http://localhost:7100/api/answer";"#,
        );
        for target in ["url", "${url}", "${process.env.HELPDESK_URL}", "`${url}`"] {
            assert_eq!(
                resolve_whole_url_target(target, &aliases, &paths).as_deref(),
                Some("${process.env.HELPDESK_URL}/api/answer"),
                "target {target} states nothing but the binding"
            );
        }
    }

    #[test]
    fn whole_url_rule_leaves_a_target_that_states_its_own_path_alone() {
        let (aliases, paths) = build_both(
            r#"const url = process.env.HELPDESK_URL ?? "http://localhost:7100/api/answer";"#,
        );
        assert_eq!(
            resolve_whole_url_target("${url}/api/other", &aliases, &paths),
            None,
            "a base with a path behind it is the existing shape, not this one"
        );
    }

    #[test]
    fn whole_url_rule_needs_a_path_in_the_fallback() {
        let (aliases, paths) = build_both(
            r#"const base = process.env.CATALOG_URL ?? "http://localhost:4001";
               const other = process.env.OTHER_URL;
               const templated = process.env.T_URL ?? `http://localhost:${port}/api/x`;"#,
        );
        assert!(
            paths.is_empty(),
            "a fallback with no path, no fallback at all, and an interpolated \
             one all state nothing: {paths:?}"
        );
        assert_eq!(resolve_whole_url_target("base", &aliases, &paths), None);
    }

    #[test]
    fn a_loopback_fallback_is_the_default_the_call_keys_on() {
        for host in [
            "localhost:3939",
            "127.0.0.1:3939",
            "0.0.0.0:3939",
            "[::1]:3939",
        ] {
            let (aliases, paths) = build_both(&format!(
                r#"const url = process.env.SERVICE_ASK_URL ?? "http://{host}/api/ask";"#
            ));
            assert_eq!(
                whole_url_local_default("url", &aliases, &paths).as_deref(),
                Some(format!("http://{host}/api/ask").as_str()),
                "a loopback default is this machine, whatever its spelling"
            );
        }
    }

    #[test]
    fn a_third_party_fallback_states_no_local_default() {
        let (aliases, paths) = build_both(
            r#"const url = process.env.PAYMENTS_URL ?? "https://api.example.com/v1/charges";"#,
        );
        assert_eq!(
            resolve_whole_url_target("url", &aliases, &paths).as_deref(),
            Some("${process.env.PAYMENTS_URL}/v1/charges"),
            "the fallback still states the path"
        );
        assert_eq!(
            whole_url_local_default("url", &aliases, &paths),
            None,
            "a third-party origin says nothing about this machine, so the call \
             keeps the verbatim key an undeclared env-var base gets"
        );
    }

    #[test]
    fn extracts_direct_process_env_alias() {
        let map = build_map(r#"const ORDERS_BASE = process.env.ORDERS_SERVICE_URL;"#);
        assert_eq!(
            map.get("ORDERS_BASE").map(String::as_str),
            Some("ORDERS_SERVICE_URL")
        );
    }

    #[test]
    fn extracts_nullish_coalescing_default_form() {
        // The exact pattern from issue #218.
        let map = build_map(
            r#"const ORDERS_BASE = process.env.ORDERS_SERVICE_URL ?? "http://localhost:3001";"#,
        );
        assert_eq!(
            map.get("ORDERS_BASE").map(String::as_str),
            Some("ORDERS_SERVICE_URL")
        );
    }

    #[test]
    fn extracts_logical_or_default_form() {
        let map = build_map(r#"const BASE = process.env.SERVICE_URL || "http://localhost:3001";"#);
        assert_eq!(map.get("BASE").map(String::as_str), Some("SERVICE_URL"));
    }

    #[test]
    fn extracts_bracket_access_form() {
        let map = build_map(r#"const BASE = process.env["SERVICE_URL"];"#);
        assert_eq!(map.get("BASE").map(String::as_str), Some("SERVICE_URL"));
    }

    #[test]
    fn extracts_let_and_var_forms() {
        let map = build_map(
            r#"let A = process.env.A_URL;
var B = process.env.B_URL ?? "";"#,
        );
        assert_eq!(map.get("A").map(String::as_str), Some("A_URL"));
        assert_eq!(map.get("B").map(String::as_str), Some("B_URL"));
    }

    #[test]
    fn unwraps_paren_nonnull_and_as_casts() {
        let map = build_map(
            r#"const A = (process.env.A_URL);
const B = process.env.B_URL!;
const C = process.env.C_URL as string;"#,
        );
        assert_eq!(map.get("A").map(String::as_str), Some("A_URL"));
        assert_eq!(map.get("B").map(String::as_str), Some("B_URL"));
        assert_eq!(map.get("C").map(String::as_str), Some("C_URL"));
    }

    #[test]
    fn ignores_non_env_bindings() {
        // Not process.env, a concatenated base, and a destructure — all out of scope.
        let map = build_map(
            r#"const HOST = config.host;
const BASE = process.env.X_URL + "/v1";
const { Y_URL } = process.env;"#,
        );
        assert!(!map.contains_key("HOST"));
        // Concatenation is intentionally not resolved (TODO scope), so BASE must
        // NOT map to a partial env name.
        assert!(!map.contains_key("BASE"));
        assert!(!map.contains_key("Y_URL"));
    }

    #[test]
    fn extracts_config_object_properties_as_dotted_keys() {
        // The corpus-3 ops-console shape: a central config object reads the
        // env vars; call targets carry `${config.catalogUrl}` bases.
        let map = build_map(
            r#"const config = {
  ordersApiUrl: process.env.ORDERS_API_URL ?? "http://localhost:4003",
  catalogUrl: process.env.CATALOG_URL ?? "http://localhost:4001",
  timeoutMs: 5000,
};"#,
        );
        assert_eq!(
            map.get("config.ordersApiUrl").map(String::as_str),
            Some("ORDERS_API_URL")
        );
        assert_eq!(
            map.get("config.catalogUrl").map(String::as_str),
            Some("CATALOG_URL")
        );
        // Non-env properties never enter the map.
        assert!(!map.contains_key("config.timeoutMs"));
    }

    #[test]
    fn extracts_config_object_through_transparent_wrappers() {
        let map = build_map(
            r#"const config = {
  base: process.env.BASE_URL,
} as const;
const cfg2 = ({ url: process.env.URL2 }) satisfies Record<string, string>;"#,
        );
        assert_eq!(map.get("config.base").map(String::as_str), Some("BASE_URL"));
        assert_eq!(map.get("cfg2.url").map(String::as_str), Some("URL2"));
    }

    #[test]
    fn config_object_property_values_unwrap_transparent_wrappers() {
        // Copilot-flagged gap on #388: wrappers on a PROPERTY VALUE (not just
        // on a direct-alias initializer) must also be stripped before the env
        // read is recognized.
        let map = build_map(
            r#"const config = {
  parens: (process.env.PARENS_URL),
  nonNull: process.env.NON_NULL_URL!,
  asString: process.env.AS_URL as string,
  asConst: process.env.CONST_URL as const,
  sat: process.env.SAT_URL satisfies string,
  wrappedDefault: (process.env.DEF_URL ?? "http://localhost:1") as string,
};"#,
        );
        assert_eq!(
            map.get("config.parens").map(String::as_str),
            Some("PARENS_URL")
        );
        assert_eq!(
            map.get("config.nonNull").map(String::as_str),
            Some("NON_NULL_URL")
        );
        assert_eq!(
            map.get("config.asString").map(String::as_str),
            Some("AS_URL")
        );
        assert_eq!(
            map.get("config.asConst").map(String::as_str),
            Some("CONST_URL")
        );
        assert_eq!(map.get("config.sat").map(String::as_str), Some("SAT_URL"));
        assert_eq!(
            map.get("config.wrappedDefault").map(String::as_str),
            Some("DEF_URL")
        );
    }

    #[test]
    fn direct_alias_unwraps_const_assertion_and_satisfies() {
        // The shared unwrap also closes the same gap on the direct-alias path.
        let map = build_map(
            r#"const A = process.env.A_URL as const;
const B = process.env.B_URL satisfies string;"#,
        );
        assert_eq!(map.get("A").map(String::as_str), Some("A_URL"));
        assert_eq!(map.get("B").map(String::as_str), Some("B_URL"));
    }

    #[test]
    fn config_object_resolves_local_alias_references() {
        // Properties referencing an earlier direct alias (long-hand and
        // shorthand) resolve through the already-collected map.
        let map = build_map(
            r#"const CATALOG_BASE = process.env.CATALOG_URL ?? "http://localhost:4001";
const catalogUrl = process.env.CATALOG_URL_ALT;
const config = { base: CATALOG_BASE, catalogUrl };"#,
        );
        assert_eq!(
            map.get("config.base").map(String::as_str),
            Some("CATALOG_URL")
        );
        assert_eq!(
            map.get("config.catalogUrl").map(String::as_str),
            Some("CATALOG_URL_ALT")
        );
    }

    #[test]
    fn config_object_skips_nested_and_dynamic_shapes() {
        // Nested objects, spreads, and computed keys need data flow — out of
        // scope, must not produce partial/wrong keys.
        let map = build_map(
            r#"const other = { x: process.env.X_URL };
const config = {
  api: { url: process.env.API_URL },
  ...other,
  ["computed"]: process.env.COMPUTED_URL,
};"#,
        );
        assert!(!map.keys().any(|k| k.starts_with("config.api")));
        assert!(!map.contains_key("config.x"));
        assert!(!map.contains_key("config.computed"));
        // The helper object itself still resolves normally.
        assert_eq!(map.get("other.x").map(String::as_str), Some("X_URL"));
    }

    fn build_exports(source: &str) -> EnvAliasMap {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

        exported_env_aliases(&module)
    }

    #[test]
    fn exports_inline_export_const_object_and_plain_alias() {
        let exports = build_exports(
            r#"export const config = { catalogUrl: process.env.CATALOG_URL ?? "x" };
export const CATALOG_BASE = process.env.CATALOG_URL;"#,
        );
        assert_eq!(
            exports.get("config.catalogUrl").map(String::as_str),
            Some("CATALOG_URL")
        );
        assert_eq!(
            exports.get("CATALOG_BASE").map(String::as_str),
            Some("CATALOG_URL")
        );
    }

    #[test]
    fn exports_follow_named_export_renames() {
        let exports = build_exports(
            r#"const config = { url: process.env.SVC_URL };
export { config as settings };"#,
        );
        assert_eq!(
            exports.get("settings.url").map(String::as_str),
            Some("SVC_URL")
        );
        assert!(!exports.contains_key("config.url"));
    }

    #[test]
    fn exports_default_object_and_default_identifier() {
        let direct = build_exports(r#"export default { url: process.env.D_URL };"#);
        assert_eq!(direct.get("default.url").map(String::as_str), Some("D_URL"));

        let via_ident = build_exports(
            r#"const config = { url: process.env.I_URL };
export default config;"#,
        );
        assert_eq!(
            via_ident.get("default.url").map(String::as_str),
            Some("I_URL")
        );
    }

    #[test]
    fn exports_ignore_reexports_and_unexported_locals() {
        let exports = build_exports(
            r#"const hidden = { url: process.env.HIDDEN_URL };
export { config } from "./elsewhere";
export * from "./other";"#,
        );
        assert!(exports.is_empty());
    }

    #[test]
    fn merge_maps_named_default_and_namespace_imports_to_local_names() {
        use crate::visitor::{ImportedSymbol, SymbolKind};

        let mut exports = EnvAliasMap::new();
        exports.insert("config.catalogUrl".to_string(), "CATALOG_URL".to_string());
        exports.insert("CATALOG_BASE".to_string(), "CATALOG_URL".to_string());
        exports.insert("default.url".to_string(), "D_URL".to_string());

        let symbol = |local: &str, imported: &str, kind: SymbolKind| ImportedSymbol {
            local_name: local.to_string(),
            imported_name: imported.to_string(),
            source: "./config".to_string(),
            kind,
        };

        let mut imported = HashMap::new();
        // Renamed named import of the config object.
        imported.insert(
            "cfg".to_string(),
            symbol("cfg", "config", SymbolKind::Named),
        );
        // Named import of a plain alias.
        imported.insert(
            "CATALOG_BASE".to_string(),
            symbol("CATALOG_BASE", "CATALOG_BASE", SymbolKind::Named),
        );
        // Default import.
        imported.insert(
            "appConfig".to_string(),
            symbol("appConfig", "appConfig", SymbolKind::Default),
        );
        // Namespace import.
        imported.insert("ns".to_string(), symbol("ns", "ns", SymbolKind::Namespace));

        let mut aliases = EnvAliasMap::new();
        // A locally-defined alias must never be clobbered by an import.
        aliases.insert("CATALOG_BASE".to_string(), "LOCAL_WINS".to_string());

        merge_imported_env_aliases(&mut aliases, &imported, |spec| {
            assert_eq!(spec, "./config");
            Some(exports.clone())
        });

        assert_eq!(
            aliases.get("cfg.catalogUrl").map(String::as_str),
            Some("CATALOG_URL")
        );
        assert_eq!(
            aliases.get("CATALOG_BASE").map(String::as_str),
            Some("LOCAL_WINS")
        );
        assert_eq!(
            aliases.get("appConfig.url").map(String::as_str),
            Some("D_URL")
        );
        assert_eq!(
            aliases.get("ns.config.catalogUrl").map(String::as_str),
            Some("CATALOG_URL")
        );
        assert_eq!(
            aliases.get("ns.CATALOG_BASE").map(String::as_str),
            Some("CATALOG_URL")
        );
    }

    #[test]
    fn resolves_dotted_config_property_target() {
        // The end shape #218's cross-file scope produces: dotted key lookup in
        // the unchanged target rewrite.
        let mut aliases = EnvAliasMap::new();
        aliases.insert("config.catalogUrl".to_string(), "CATALOG_URL".to_string());

        assert_eq!(
            resolve_target_env_alias("${config.catalogUrl}/api/v2/products/${id}", &aliases)
                .as_deref(),
            Some("${process.env.CATALOG_URL}/api/v2/products/${id}")
        );
    }

    #[test]
    fn resolves_leading_alias_in_target() {
        let mut aliases = EnvAliasMap::new();
        aliases.insert("ORDERS_BASE".to_string(), "ORDERS_SERVICE_URL".to_string());

        assert_eq!(
            resolve_target_env_alias("${ORDERS_BASE}/orders/${orderId}", &aliases).as_deref(),
            Some("${process.env.ORDERS_SERVICE_URL}/orders/${orderId}")
        );
    }

    #[test]
    fn resolves_leading_alias_past_wrapper_backtick() {
        let mut aliases = EnvAliasMap::new();
        aliases.insert("ORDERS_BASE".to_string(), "ORDERS_SERVICE_URL".to_string());

        assert_eq!(
            resolve_target_env_alias("`${ORDERS_BASE}/orders/${id}`", &aliases).as_deref(),
            Some("`${process.env.ORDERS_SERVICE_URL}/orders/${id}`")
        );
    }

    #[test]
    fn leaves_unknown_and_mid_path_interpolations_untouched() {
        let mut aliases = EnvAliasMap::new();
        aliases.insert("ORDERS_BASE".to_string(), "ORDERS_SERVICE_URL".to_string());

        // Unknown leading var: not an alias.
        assert!(resolve_target_env_alias("${API_URL}/users", &aliases).is_none());
        // A path parameter mid-URL must never be treated as a base-URL alias.
        assert!(resolve_target_env_alias("/orders/${ORDERS_BASE}", &aliases).is_none());
        // Empty alias map short-circuits.
        assert!(resolve_target_env_alias("${ORDERS_BASE}/x", &EnvAliasMap::new()).is_none());
    }
}
