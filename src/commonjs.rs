//! What a CommonJS module binds and what it publishes (carrick#1348).
//!
//! Call resolution reads two tables: the local bindings a file introduced from
//! elsewhere, and the names each module publishes. Both were read from ESM
//! declarations only, so a callee reached the other way recorded no edge
//! however plainly the source named it:
//!
//! ```ignore
//! // helpers.js
//! function computeTotal(items) { /* … */ }
//! module.exports = { computeTotal };
//!
//! // index.js
//! const { computeTotal } = require("./helpers");
//! function handleOrder(order) { return computeTotal(order.items); }
//! ```
//!
//! `handleOrder` calls `computeTotal`, and `get_callers` on `computeTotal`
//! answered nobody. This module reads the same two facts out of the assignment
//! forms CommonJS states them in. Purely structural — a binding form and an
//! assignment target, no package names and no naming heuristics — and it says
//! nothing the source does not: a specifier that is not a literal string names
//! no module, and is counted as a limit rather than guessed at.
//!
//! **Read on the binding side**, at MODULE SCOPE only, which is the scope an
//! ESM import has:
//!
//! - `const { x } = require("./m")` and `const { a: b } = require("./m")` — a
//!   named binding, exactly like `import { a as b }`.
//! - `const m = require("./m")` — the whole module under one name, like
//!   `import * as m`. `m.x()` is then `./m`'s own `x`.
//! - `const x = require("./m").x` — one member, named.
//! - `import x = require("./m")` — the TypeScript spelling of the same thing.
//!
//! A require inside a function body binds a name in that function, and two
//! functions in one file may bind the same name to different modules. The
//! per-file table has one entry per name, so a function-scope require is not
//! read into it rather than letting one of them answer for the other
//! (carrick#1352). The lexical receiver walk in [`crate::visitor`] does key by
//! scope, so a MEMBER call on such a binding still resolves where the module
//! it names is readable.
//!
//! One form resolves only where the names agree: `const fn =
//! require("./m"); fn()` is a whole-module binding CALLED, and it is looked up
//! as `./m`'s own `fn`. A module that publishes its function under another
//! name (`module.exports = otherName`) records no edge for it.
//!
//! **Read on the export side**: `module.exports = { … }`, `module.exports =
//! name`, `module.exports.x = …`, `exports.x = …`, and `module.exports =
//! require("./other")`, which republishes another module's table the way
//! `export * from` does.
//!
//! Reference: `docs/reference/module-resolution.md`.

use std::collections::HashMap;

use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::import_bindings::DEFAULT_EXPORT;
use crate::visitor::{ImportedSymbol, SymbolKind};

/// The identifier a CommonJS require is written as.
const REQUIRE: &str = "require";
/// The module object a CommonJS file assigns its exports onto.
const MODULE: &str = "module";
/// The exports object, both as `module.exports` and on its own.
const EXPORTS: &str = "exports";

/// What one module's `require` calls bind, and what could not be read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RequireBindings {
    /// Local binding → the require that introduced it, in the same shape an
    /// ESM import produces, so call resolution treats the two identically.
    pub bindings: HashMap<String, ImportedSymbol>,
    /// `require(expr)` calls whose specifier is not a literal string, anywhere
    /// in the file. Nothing in the source says which module they load, so they
    /// are counted and reported rather than guessed at.
    pub computed_specifiers: usize,
}

/// Every module-scope binding `module` introduces through `require`, plus the
/// count of requires whose specifier is computed.
pub fn require_bindings(module: &Module) -> RequireBindings {
    let mut found = RequireBindings::default();

    for item in &module.body {
        match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => {
                for declarator in &var.decls {
                    let Some(init) = declarator.init.as_deref() else {
                        continue;
                    };
                    record_declarator(&mut found.bindings, &declarator.name, init);
                }
            }
            // `import x = require("./m")`: TypeScript's spelling, a whole
            // module under one name.
            ModuleItem::ModuleDecl(ModuleDecl::TsImportEquals(decl)) => {
                let TsModuleRef::TsExternalModuleRef(external) = &decl.module_ref else {
                    continue;
                };
                let local_name = decl.id.sym.to_string();
                found.bindings.insert(
                    local_name.clone(),
                    ImportedSymbol {
                        imported_name: local_name.clone(),
                        local_name,
                        source: external.expr.value.to_string(),
                        kind: SymbolKind::Namespace,
                    },
                );
            }
            _ => {}
        }
    }

    let mut computed = ComputedSpecifiers::default();
    module.visit_with(&mut computed);
    found.computed_specifiers = computed.count;
    found
}

/// One name a require declarator binds.
pub struct RequireBound<'a> {
    /// The binding as written, so a caller that needs its lexical identity
    /// (the receiver walk in [`crate::visitor`]) can take it.
    pub local: &'a BindingIdent,
    /// The name the required module publishes it under.
    pub imported: String,
    /// [`SymbolKind::Namespace`] for a binding that names the whole module.
    pub kind: SymbolKind,
}

/// The names one `const … = require("…")` binds, whatever its pattern.
///
/// The single reader of require BINDING FORMS: the per-file import table and
/// the lexical receiver walk both build from this list, so neither can drift
/// into supporting a form the other does not.
pub fn require_bound_names(name: &Pat) -> Vec<RequireBound<'_>> {
    match name {
        // `const m = require("./m")` — the module itself.
        Pat::Ident(local) => vec![RequireBound {
            local,
            imported: local.id.sym.to_string(),
            kind: SymbolKind::Namespace,
        }],
        // `const { x, a: b } = require("./m")` — named bindings.
        Pat::Object(pattern) => pattern
            .props
            .iter()
            .filter_map(|property| match property {
                // `{ x }`, `{ x = fallback }`
                ObjectPatProp::Assign(shorthand) => Some(RequireBound {
                    local: &shorthand.key,
                    imported: shorthand.key.id.sym.to_string(),
                    kind: SymbolKind::Named,
                }),
                // `{ a: b }`. A nested pattern binds parts of a member, not
                // the member, so it names no importable symbol.
                ObjectPatProp::KeyValue(entry) => {
                    let Pat::Ident(local) = entry.value.as_ref() else {
                        return None;
                    };
                    Some(RequireBound {
                        local,
                        imported: pattern_key(&entry.key)?,
                        kind: SymbolKind::Named,
                    })
                }
                // `{ ...rest }` names nothing in particular.
                ObjectPatProp::Rest(_) => None,
            })
            .collect(),
        // An array pattern destructures positionally: position names no export.
        _ => Vec::new(),
    }
}

/// One `const … = <init>` at module scope.
fn record_declarator(bindings: &mut HashMap<String, ImportedSymbol>, name: &Pat, init: &Expr) {
    // `const x = require("./m").x` — one member of the module, named.
    if let Expr::Member(member) = init
        && let Some(source) = require_specifier(&member.obj)
        && let MemberProp::Ident(property) = &member.prop
        && let Pat::Ident(local) = name
    {
        let local_name = local.id.sym.to_string();
        bindings.insert(
            local_name.clone(),
            ImportedSymbol {
                local_name,
                imported_name: property.sym.to_string(),
                source,
                kind: SymbolKind::Named,
            },
        );
        return;
    }

    let Some(source) = require_specifier(init) else {
        return;
    };

    for bound in require_bound_names(name) {
        let local_name = bound.local.id.sym.to_string();
        bindings.insert(
            local_name.clone(),
            ImportedSymbol {
                local_name,
                imported_name: bound.imported,
                source: source.clone(),
                kind: bound.kind,
            },
        );
    }
}

/// The literal specifier of `require("…")`, or `None` for anything else —
/// including `require(expr)`, which names no module.
pub fn require_specifier(expr: &Expr) -> Option<String> {
    let Expr::Call(call) = expr else {
        return None;
    };
    if !is_require_callee(&call.callee) {
        return None;
    }
    literal_specifier(call)
}

fn is_require_callee(callee: &Callee) -> bool {
    matches!(callee, Callee::Expr(expr) if matches!(expr.as_ref(), Expr::Ident(ident) if ident.sym == *REQUIRE))
}

/// The name an object-pattern key states, for the two forms that state one.
fn pattern_key(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(literal) => Some(literal.value.to_string()),
        _ => None,
    }
}

/// Counts `require(expr)` anywhere in a file: the calls that name no module.
#[derive(Default)]
struct ComputedSpecifiers {
    count: usize,
}

impl Visit for ComputedSpecifiers {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if is_require_callee(&call.callee) && literal_specifier(call).is_none() {
            self.count += 1;
        }
        call.visit_children_with(self);
    }
}

/// The single string-literal argument of a call, where it has exactly one.
fn literal_specifier(call: &CallExpr) -> Option<String> {
    let [argument] = call.args.as_slice() else {
        return None;
    };
    if argument.spread.is_some() {
        return None;
    }
    match argument.expr.as_ref() {
        Expr::Lit(Lit::Str(specifier)) => Some(specifier.value.to_string()),
        _ => None,
    }
}

/// What a module publishes through CommonJS assignments.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CommonJsExports {
    /// Exported name → the local binding it names in THIS module, in the shape
    /// the ESM export table uses. `None` where the assignment publishes a
    /// value with no name of its own.
    pub local: HashMap<String, Option<String>>,
    /// `module.exports = require("./other")`: a specifier whose whole table
    /// this module republishes, like `export * from`.
    pub stars: Vec<String>,
}

/// Every name `module` publishes through `module.exports` or `exports`.
pub fn export_assignments(module: &Module) -> CommonJsExports {
    let mut exports = CommonJsExports::default();

    for item in &module.body {
        let ModuleItem::Stmt(Stmt::Expr(statement)) = item else {
            continue;
        };
        let Expr::Assign(assignment) = statement.expr.as_ref() else {
            continue;
        };
        if assignment.op != op!("=") {
            continue;
        }
        let AssignTarget::Simple(SimpleAssignTarget::Member(target)) = &assignment.left else {
            continue;
        };

        match assignment_target(target) {
            // `module.exports = …` — the module's whole published value.
            Some(ExportTarget::Whole) => record_whole_exports(&mut exports, &assignment.right),
            // `module.exports.x = …`, `exports.x = …` — one name.
            Some(ExportTarget::Member(name)) => {
                exports.local.insert(name, local_name_of(&assignment.right));
            }
            None => {}
        }
    }

    exports
}

/// Which published name an assignment target names.
enum ExportTarget {
    Whole,
    Member(String),
}

fn assignment_target(target: &MemberExpr) -> Option<ExportTarget> {
    let MemberProp::Ident(property) = &target.prop else {
        return None;
    };
    match target.obj.as_ref() {
        // `module.exports = …`
        Expr::Ident(object) if object.sym == *MODULE && property.sym == *EXPORTS => {
            Some(ExportTarget::Whole)
        }
        // `exports.x = …`
        Expr::Ident(object) if object.sym == *EXPORTS => {
            Some(ExportTarget::Member(property.sym.to_string()))
        }
        // `module.exports.x = …`
        Expr::Member(inner) => {
            let Expr::Ident(object) = inner.obj.as_ref() else {
                return None;
            };
            let MemberProp::Ident(exports) = &inner.prop else {
                return None;
            };
            (object.sym == *MODULE && exports.sym == *EXPORTS)
                .then(|| ExportTarget::Member(property.sym.to_string()))
        }
        _ => None,
    }
}

/// `module.exports = <value>`: an object literal publishes one name per
/// property; anything else publishes a single value, which is what a default
/// export is.
fn record_whole_exports(exports: &mut CommonJsExports, value: &Expr) {
    if let Expr::Object(literal) = value {
        for property in &literal.props {
            let PropOrSpread::Prop(property) = property else {
                // `{ ...others }` republishes a value this pass cannot name.
                continue;
            };
            match property.as_ref() {
                // `{ computeTotal }`
                Prop::Shorthand(ident) => {
                    let name = ident.sym.to_string();
                    exports.local.insert(name.clone(), Some(name));
                }
                // `{ total: computeTotal }`, `{ total: () => … }`
                Prop::KeyValue(entry) => {
                    let Some(name) = pattern_key(&entry.key) else {
                        continue;
                    };
                    exports.local.insert(name, local_name_of(&entry.value));
                }
                // `{ total() { … } }`
                Prop::Method(method) => {
                    let Some(name) = pattern_key(&method.key) else {
                        continue;
                    };
                    exports.local.insert(name, None);
                }
                _ => {}
            }
        }
        return;
    }

    // `module.exports = require("./other")` republishes another module's whole
    // table, which is what `export * from` means.
    if let Some(specifier) = require_specifier(value) {
        exports.stars.push(specifier);
        return;
    }

    exports
        .local
        .insert(DEFAULT_EXPORT.to_string(), local_name_of(value));
}

/// The local binding an exported value names, where it names one.
fn local_name_of(value: &Expr) -> Option<String> {
    match value {
        Expr::Ident(ident) => Some(ident.sym.to_string()),
        Expr::Fn(function) => function.ident.as_ref().map(|ident| ident.sym.to_string()),
        Expr::Class(class) => class.ident.as_ref().map(|ident| ident.sym.to_string()),
        _ => None,
    }
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

    fn parse(source: &str) -> Module {
        parse_as("module.js", source)
    }

    fn parse_as(name: &str, source: &str) -> Module {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(name);
        std::fs::write(&path, source).expect("write fixture");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        parse_file(&path, &cm, &handler).expect("fixture parses")
    }

    fn binding(source: &str, local: &str) -> Option<ImportedSymbol> {
        require_bindings(&parse(source))
            .bindings
            .get(local)
            .cloned()
    }

    #[test]
    fn destructured_require_binds_each_name() {
        let symbol = binding(
            "const { computeTotal } = require('./helpers');",
            "computeTotal",
        )
        .expect("computeTotal is bound");
        assert_eq!(symbol.imported_name, "computeTotal");
        assert_eq!(symbol.source, "./helpers");
        assert_eq!(symbol.kind, SymbolKind::Named);
    }

    #[test]
    fn renamed_destructure_keeps_both_names() {
        let symbol = binding(
            "const { computeTotal: total } = require('./helpers');",
            "total",
        )
        .expect("the local name is bound");
        assert_eq!(symbol.imported_name, "computeTotal");
        assert_eq!(symbol.local_name, "total");
    }

    #[test]
    fn a_whole_module_binding_is_a_namespace() {
        let symbol =
            binding("const helpers = require('./helpers');", "helpers").expect("helpers is bound");
        assert_eq!(symbol.kind, SymbolKind::Namespace);
        assert_eq!(symbol.source, "./helpers");
    }

    #[test]
    fn a_member_of_a_require_is_a_named_binding() {
        let symbol = binding("const total = require('./helpers').computeTotal;", "total")
            .expect("total is bound");
        assert_eq!(symbol.imported_name, "computeTotal");
        assert_eq!(symbol.kind, SymbolKind::Named);
    }

    #[test]
    fn typescript_import_equals_binds_the_module() {
        let module = parse_as("module.ts", "import helpers = require('./helpers');");
        let symbol = require_bindings(&module)
            .bindings
            .get("helpers")
            .cloned()
            .expect("helpers is bound");
        assert_eq!(symbol.kind, SymbolKind::Namespace);
        assert_eq!(symbol.source, "./helpers");
    }

    /// A `.ts` file states a require exactly as a `.js` one does — the
    /// extension was measured not to be the discriminator (carrick#1348).
    #[test]
    fn a_require_in_a_typescript_file_binds_the_same_names() {
        let module = parse_as(
            "module.ts",
            "const { computeTotal } = require('./helpers');\nexport function use(o: number[]) { return computeTotal(o); }\n",
        );
        let symbol = require_bindings(&module)
            .bindings
            .get("computeTotal")
            .cloned()
            .expect("computeTotal is bound");
        assert_eq!(symbol.imported_name, "computeTotal");
        assert_eq!(symbol.kind, SymbolKind::Named);
    }

    #[test]
    fn a_function_scope_require_binds_nothing() {
        let found = require_bindings(&parse(
            "function load() { const { computeTotal } = require('./helpers'); return computeTotal; }",
        ));
        assert!(
            found.bindings.is_empty(),
            "a function-scope binding is not the file's: {:?}",
            found.bindings
        );
    }

    #[test]
    fn a_computed_specifier_binds_nothing_and_is_counted() {
        let found = require_bindings(&parse(
            "const name = './helpers';\nconst helpers = require(name);\n",
        ));
        assert!(found.bindings.is_empty());
        assert_eq!(found.computed_specifiers, 1);
    }

    #[test]
    fn a_literal_require_is_not_counted_as_computed() {
        let found = require_bindings(&parse("const helpers = require('./helpers');"));
        assert_eq!(found.computed_specifiers, 0);
    }

    #[test]
    fn an_object_export_publishes_each_property() {
        let exports = export_assignments(&parse(
            "function computeTotal() {}\nmodule.exports = { computeTotal, total: computeTotal };\n",
        ));
        assert_eq!(
            exports.local.get("computeTotal"),
            Some(&Some("computeTotal".to_string()))
        );
        assert_eq!(
            exports.local.get("total"),
            Some(&Some("computeTotal".to_string()))
        );
    }

    #[test]
    fn a_member_assignment_publishes_one_name() {
        let exports = export_assignments(&parse(
            "exports.computeTotal = computeTotal;\nmodule.exports.handleOrder = handleOrder;\n",
        ));
        assert_eq!(
            exports.local.get("computeTotal"),
            Some(&Some("computeTotal".to_string()))
        );
        assert_eq!(
            exports.local.get("handleOrder"),
            Some(&Some("handleOrder".to_string()))
        );
    }

    #[test]
    fn a_whole_value_export_publishes_a_default() {
        let exports = export_assignments(&parse(
            "function computeTotal() {}\nmodule.exports = computeTotal;\n",
        ));
        assert_eq!(
            exports.local.get(DEFAULT_EXPORT),
            Some(&Some("computeTotal".to_string()))
        );
    }

    #[test]
    fn re_exporting_a_require_republishes_its_table() {
        let exports = export_assignments(&parse("module.exports = require('./other');"));
        assert_eq!(exports.stars, vec!["./other".to_string()]);
        assert!(exports.local.is_empty());
    }

    /// The analyzer's import table and the framework-detect sample are built
    /// from `ImportSymbolExtractor` alone. A require binding must never reach
    /// them: the prompt bytes of a CommonJS file would move, and its cached
    /// answer would stop replaying.
    #[test]
    fn require_bindings_stay_out_of_the_esm_import_table() {
        let module = parse(
            "const { computeTotal } = require('./helpers');\nconst helpers = require('./helpers');\n",
        );
        let mut extractor = crate::visitor::ImportSymbolExtractor::new();
        module.visit_with(&mut extractor);
        assert!(
            extractor.imported_symbols.is_empty(),
            "the ESM import table must not see require bindings: {:?}",
            extractor.imported_symbols
        );
        assert_eq!(require_bindings(&module).bindings.len(), 2);
    }
}
