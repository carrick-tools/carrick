//! The HTTP verbs a type annotation says a value can be (carrick#2049).
//!
//! A request whose method is a parameter (`async function send(method:
//! "POST" | "DELETE", …) { await fetch(url, { method }) }`) sends one of the
//! verbs the parameter is declared with. The annotation is the source's own
//! statement of every method the request can carry, so it is read as exactly
//! that: a union of string literals, each an HTTP verb, written inline or
//! through a type alias the same file declares.
//!
//! Anything else is no statement. A parameter typed `string`, a union with a
//! member that is not a verb, an alias the file does not declare (an import),
//! an alias a generic parameter shadows, and a type the reader cannot see
//! through (`keyof typeof VERBS`, `Method[number]`) all read as `None`, so
//! the request keeps whatever the rest of the scanner says of it.

use std::collections::{BTreeSet, HashMap, HashSet};

use swc_ecma_ast::*;

use crate::type_manifest::is_http_method;

/// How many alias hops a union is followed through (`type A = B`, `type B =
/// "GET" | "POST"`).
const ALIAS_DEPTH: usize = 3;

/// The type aliases a module declares at its top level that name a closed
/// set of verbs, by name. An alias declared more than once is dropped, since
/// nothing here says which declaration a use means.
#[derive(Debug, Default)]
pub struct TypeAliases {
    verbs: HashMap<String, Vec<String>>,
}

impl TypeAliases {
    pub fn of(module: &Module) -> Self {
        let mut declared: HashMap<String, &TsType> = HashMap::new();
        let mut repeated: BTreeSet<String> = BTreeSet::new();
        for item in &module.body {
            let decl = match item {
                ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
                _ => continue,
            };
            let Decl::TsTypeAlias(alias) = decl else {
                continue;
            };
            // A generic alias (`type Verbs<T> = …`) is a function of its
            // arguments, not a fixed set.
            if alias.type_params.is_some() {
                continue;
            }
            let name = alias.id.sym.to_string();
            if declared.insert(name.clone(), &alias.type_ann).is_some() {
                repeated.insert(name);
            }
        }
        for name in repeated {
            declared.remove(&name);
        }
        // An alias of an alias resolves once the inner one has. A cycle
        // never does.
        let mut aliases = Self::default();
        for _ in 0..=ALIAS_DEPTH {
            for (name, ty) in &declared {
                if !aliases.verbs.contains_key(name)
                    && let Some(verbs) = aliases.verbs_of(ty, &HashSet::new())
                {
                    aliases.verbs.insert(name.clone(), verbs);
                }
            }
        }
        aliases
    }

    /// The verbs `ty` says a value can be, sorted. `shadowed` are the type
    /// names the function declares itself (its type parameters), which mean
    /// something else than a module alias of the same name.
    pub fn verbs_of(&self, ty: &TsType, shadowed: &HashSet<String>) -> Option<Vec<String>> {
        let mut verbs = BTreeSet::new();
        self.collect(ty, shadowed, &mut verbs)?;
        (!verbs.is_empty()).then(|| verbs.into_iter().collect())
    }

    fn collect(
        &self,
        ty: &TsType,
        shadowed: &HashSet<String>,
        verbs: &mut BTreeSet<String>,
    ) -> Option<()> {
        match ty {
            TsType::TsParenthesizedType(paren) => self.collect(&paren.type_ann, shadowed, verbs),
            TsType::TsLitType(literal) => match &literal.lit {
                TsLit::Str(text) => {
                    let verb = text.value.to_string_lossy().into_owned();
                    is_http_method(&verb).then(|| {
                        verbs.insert(verb.trim().to_uppercase());
                    })
                }
                _ => None,
            },
            TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(union)) => {
                union
                    .types
                    .iter()
                    .try_for_each(|member| self.collect(member, shadowed, verbs))
            }
            TsType::TsTypeRef(reference) => {
                let TsEntityName::Ident(name) = &reference.type_name else {
                    return None;
                };
                if reference.type_params.is_some() || shadowed.contains(name.sym.as_ref()) {
                    return None;
                }
                verbs.extend(self.verbs.get(name.sym.as_ref())?.iter().cloned());
                Some(())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_common::{FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax};

    fn parse(source: &str) -> Module {
        let map: Lrc<SourceMap> = Default::default();
        let file = map.new_source_file(FileName::Anon.into(), source.to_string());
        Parser::new(
            Syntax::Typescript(TsSyntax::default()),
            StringInput::from(&*file),
            None,
        )
        .parse_module()
        .expect("the source parses")
    }

    /// The verbs read from the annotation of the first parameter of the
    /// module's last function.
    fn verbs(source: &str) -> Option<Vec<String>> {
        let module = parse(source);
        let aliases = TypeAliases::of(&module);
        let function = module
            .body
            .iter()
            .rev()
            .find_map(|item| match item {
                ModuleItem::Stmt(Stmt::Decl(Decl::Fn(function))) => Some(function),
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                    Decl::Fn(function) => Some(function),
                    _ => None,
                },
                _ => None,
            })
            .expect("a function");
        let Pat::Ident(param) = &function.function.params[0].pat else {
            panic!("a plain parameter");
        };
        let ty = &param.type_ann.as_ref().expect("an annotation").type_ann;
        let shadowed =
            crate::forwarded_body::type_param_names(function.function.type_params.as_deref());
        aliases.verbs_of(ty, &shadowed)
    }

    fn some(verbs: &[&str]) -> Option<Vec<String>> {
        Some(verbs.iter().map(|verb| verb.to_string()).collect())
    }

    #[test]
    fn an_inline_union_of_verbs_is_read() {
        assert_eq!(
            verbs(r#"function f(m: "POST" | "DELETE") {}"#),
            some(&["DELETE", "POST"])
        );
        assert_eq!(verbs(r#"function f(m: "put") {}"#), some(&["PUT"]));
    }

    #[test]
    fn an_alias_the_file_declares_is_followed() {
        assert_eq!(
            verbs(r#"type Verb = "POST" | "PATCH"; function f(m: Verb) {}"#),
            some(&["PATCH", "POST"])
        );
        assert_eq!(
            verbs(
                r#"export type Write = "POST" | "PUT"; type Verb = Write | "DELETE"; function f(m: Verb) {}"#
            ),
            some(&["DELETE", "POST", "PUT"])
        );
    }

    #[test]
    fn a_type_that_is_not_a_closed_set_of_verbs_says_nothing() {
        for source in [
            // A string states nothing.
            r#"function f(m: string) {}"#,
            // One member that is not a verb.
            r#"function f(m: "POST" | "SUBSCRIBE") {}"#,
            // A member the reader cannot see through.
            r#"function f(m: "POST" | string) {}"#,
            r#"function f(m: "POST" | undefined) {}"#,
            // An import is no declaration here.
            r#"import type { Verb } from "./verbs"; function f(m: Verb) {}"#,
            // A generic alias, an alias declared twice, an alias a type
            // parameter shadows, a cycle.
            r#"type Verb<T> = "POST"; function f(m: Verb<1>) {}"#,
            r#"type Verb = "POST"; type Verb = "DELETE"; function f(m: Verb) {}"#,
            r#"type Verb = "POST"; function f<Verb extends string>(m: Verb) {}"#,
            r#"type A = B; type B = A; function f(m: A) {}"#,
        ] {
            assert_eq!(verbs(source), None, "{source}");
        }
    }
}
