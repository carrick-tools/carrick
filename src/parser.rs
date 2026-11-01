use std::fs;
use std::path::Path;
use swc_common::{FileName, GLOBALS, Globals, Mark, SourceMap, errors::Handler, sync::Lrc};
use swc_ecma_ast::Module;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use swc_ecma_transforms_base::resolver;
use swc_ecma_visit::VisitMutWith;
use tracing::warn;

/// The parser configuration a file's extension calls for, and whether that
/// configuration is TypeScript. The single definition for every parse in the
/// scanner: a file read one way by one pass and another way by the next is a
/// file two passes disagree about.
///
/// Two facts decide it, and nothing else:
///
/// - **TypeScript or ES**, from `.ts`/`.tsx`/`.mts`/`.cts` against everything
///   else. `.mts`/`.cts` never hold JSX, as `.ts` does not (carrick#904).
///   Decorators are enabled on the TypeScript arms so a decorated class parses
///   into `Decorator` nodes instead of failing outright.
/// - **JSX**, which is enabled for every ES parse (carrick#803). Keying JSX on
///   the extension read `.jsx` and refused `.js`, and a `.js` file holding JSX
///   is the entry point every React Native app is scaffolded with — the file
///   failed to parse and was excluded from the index. JSX is unambiguous in ES
///   mode, where there is no type assertion for `<` to be read as, so enabling
///   it costs a plain `.js` file nothing. The TypeScript arms cannot do the
///   same: there `<T>x` is a type assertion, and `tsx` genuinely changes how a
///   valid file parses, which is why the extension still decides it.
pub(crate) fn syntax_for_path(file_path: &Path) -> (Syntax, bool) {
    match file_path.extension().and_then(|ext| ext.to_str()) {
        Some("ts" | "mts" | "cts") => (
            Syntax::Typescript(TsSyntax {
                decorators: true,
                ..Default::default()
            }),
            true,
        ),
        Some("tsx") => (
            Syntax::Typescript(TsSyntax {
                tsx: true,
                decorators: true,
                ..Default::default()
            }),
            true,
        ),
        _ => (
            Syntax::Es(EsSyntax {
                jsx: true,
                ..Default::default()
            }),
            false,
        ),
    }
}

/// Reads one fact out of a module: the specifiers it imports from.
///
/// It owns a source map and a quiet diagnostic handler, as
/// [`crate::import_bindings::BindingResolver`] does, so a caller reading many
/// files pays for that setup once. A file that does not parse contributes
/// nothing rather than failing the pass — the caller is answering "what does
/// this member import", and an unparseable file is an unanswerable part of it.
///
/// Static specifiers only: `import`, `export ... from` and `export * from`.
/// A `await import(expr)` names its target inside an expression and is not
/// read here; nothing in this scanner depends on it being.
pub struct ModuleReader {
    source_map: Lrc<SourceMap>,
    handler: Handler,
}

impl Default for ModuleReader {
    fn default() -> Self {
        let source_map: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            false,
            false,
            Some(source_map.clone()),
        );
        Self {
            source_map,
            handler,
        }
    }
}

impl ModuleReader {
    /// A reader that prints nothing about a file that does not parse: for a
    /// read made ahead of the pass that reports it (the source walk,
    /// carrick#1902).
    pub fn quiet() -> Self {
        Self {
            source_map: Default::default(),
            handler: Handler::with_emitter_writer(Box::new(std::io::sink()), None),
        }
    }

    /// Every module specifier `file` loads a module by, type-only or not
    /// (carrick#1902): what [`Self::import_specifiers`] reads, plus
    /// `import x = require()` and `require` or `import()` with a literal
    /// specifier anywhere in the file. A specifier the source computes names
    /// no module and is not here.
    pub fn loaded_specifiers(&mut self, file: &Path) -> Vec<String> {
        let Some(module) = parse_file(file, &self.source_map, &self.handler) else {
            return Vec::new();
        };
        module_loads(&module)
    }

    /// [`Self::loaded_specifiers`] and, after them, what `file` reads in type
    /// position ([`type_reads`], [`reference_directives`]), from one parse:
    /// the reach walk's reads of a file.
    pub fn reach_specifiers(&mut self, file: &Path) -> Vec<String> {
        let Some(source) = read_source(file) else {
            return Vec::new();
        };
        let name = FileName::Real(file.to_path_buf());
        let Some(module) =
            parse_source(file, name, source.clone(), &self.source_map, &self.handler)
        else {
            return Vec::new();
        };
        reach_of(&module, &source)
    }

    /// What a copy of `file` held as text (the one a commit holds, say) reads
    /// and declares, for a reader that asks whether an edit moved the
    /// module's edges (carrick#3114). `label` keeps two copies of one path
    /// apart in the source map. `None` when the text does not parse.
    pub fn module_shape_of_text(
        &mut self,
        file: &Path,
        label: &str,
        text: String,
    ) -> Option<ModuleShape> {
        let module = parse_source(
            file,
            FileName::Custom(label.to_string()),
            text.clone(),
            &self.source_map,
            &self.handler,
        )?;
        Some(ModuleShape {
            reach: reach_of(&module, &text).into_iter().collect(),
            is_module: is_module(&module, is_javascript(file)),
            augments_types: augments_types(&module),
        })
    }

    /// Every module specifier `file` imports from, in source order.
    ///
    /// Type-only imports are included: a package imported for its types alone
    /// is still imported, and a shared types package is exactly the member a
    /// dependents list exists to name.
    pub fn import_specifiers(&mut self, file: &Path) -> Vec<String> {
        let Some(module) = parse_file(file, &self.source_map, &self.handler) else {
            return Vec::new();
        };
        static_specifiers(&module)
    }
}

/// What one copy of a source file says about the edges it adds to a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleShape {
    /// What the file loads, as the reach walk reads it
    /// ([`ModuleReader::reach_specifiers`]), as a set.
    pub reach: std::collections::BTreeSet<String>,
    /// An import, an export, or in JavaScript a CommonJS `require`,
    /// `module.exports` or `exports.`; any other file is a script, whose
    /// top-level declarations are globals.
    pub is_module: bool,
    /// A `declare global`, a `declare module` or a `declare namespace`, which
    /// change types program-wide.
    pub augments_types: bool,
}

/// [`ModuleReader::reach_specifiers`] over a parsed module and its text.
fn reach_of(module: &Module, source: &str) -> Vec<String> {
    let mut found = module_loads(module);
    found.extend(type_reads(module));
    found.extend(reference_directives(source));
    found
}

fn is_javascript(file: &Path) -> bool {
    file.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "js" | "jsx" | "mjs" | "cjs"))
}

fn is_module(module: &Module, javascript: bool) -> bool {
    use swc_ecma_ast::{Callee, Expr, MemberExpr, ModuleItem};
    use swc_ecma_visit::{Visit, VisitWith};
    if module
        .body
        .iter()
        .any(|item| matches!(item, ModuleItem::ModuleDecl(_)))
    {
        return true;
    }
    if !javascript {
        return false;
    }
    struct CommonJs(bool);
    impl Visit for CommonJs {
        fn visit_call_expr(&mut self, call: &swc_ecma_ast::CallExpr) {
            if let Callee::Expr(callee) = &call.callee
                && matches!(&**callee, Expr::Ident(ident) if ident.sym == *"require")
            {
                self.0 = true;
            }
            call.visit_children_with(self);
        }
        fn visit_member_expr(&mut self, member: &MemberExpr) {
            if let Expr::Ident(object) = &*member.obj
                && (object.sym == *"exports" || object.sym == *"module")
            {
                self.0 = true;
            }
            member.visit_children_with(self);
        }
    }
    let mut found = CommonJs(false);
    module.visit_with(&mut found);
    found.0
}

fn augments_types(module: &Module) -> bool {
    use swc_ecma_ast::TsModuleDecl;
    use swc_ecma_visit::{Visit, VisitWith};
    struct Augments(bool);
    impl Visit for Augments {
        fn visit_ts_module_decl(&mut self, decl: &TsModuleDecl) {
            if decl.global || decl.declare {
                self.0 = true;
            }
            decl.visit_children_with(self);
        }
    }
    let mut found = Augments(false);
    module.visit_with(&mut found);
    found.0
}

/// Every module specifier `module` loads a module by, as
/// [`ModuleReader::loaded_specifiers`] reads them from a file.
pub fn module_loads(module: &Module) -> Vec<String> {
    use swc_ecma_ast::{CallExpr, Callee, Expr, ModuleDecl, ModuleItem, TsModuleRef};
    use swc_ecma_visit::{Visit, VisitWith};
    struct Loads(Vec<String>);
    impl Visit for Loads {
        fn visit_call_expr(&mut self, call: &CallExpr) {
            let loads = match &call.callee {
                Callee::Import(_) => true,
                Callee::Expr(callee) => {
                    matches!(&**callee, Expr::Ident(ident) if ident.sym == *"require")
                }
                Callee::Super(_) => false,
            };
            if loads
                && let [argument] = call.args.as_slice()
                && argument.spread.is_none()
                && let Some(specifier) = crate::request_summary::literal_specifier(&argument.expr)
            {
                self.0.push(specifier);
            }
            call.visit_children_with(self);
        }
    }
    let mut found = static_specifiers(module);
    found.extend(module.body.iter().filter_map(|item| match item {
        ModuleItem::ModuleDecl(ModuleDecl::TsImportEquals(decl)) => match &decl.module_ref {
            TsModuleRef::TsExternalModuleRef(external) => {
                Some(external.expr.value.to_string_lossy().into_owned())
            }
            TsModuleRef::TsEntityName(_) => None,
        },
        _ => None,
    }));
    let mut loads = Loads(Vec::new());
    module.visit_with(&mut loads);
    found.extend(loads.0);
    found
}

/// The specifiers of every `import('./x').T` and `typeof import('./x')` type
/// in `module`. [`module_loads`] does not read them: it feeds passes that ask
/// what a file loads at run time. Only the change-scope reach walk asks what a
/// file reads for its types (a type-position import is an edge for reach).
pub fn type_reads(module: &Module) -> Vec<String> {
    use swc_ecma_ast::TsImportType;
    use swc_ecma_visit::{Visit, VisitWith};
    struct Reads(Vec<String>);
    impl Visit for Reads {
        fn visit_ts_import_type(&mut self, import: &TsImportType) {
            self.0.push(import.arg.value.to_string_lossy().into_owned());
            import.visit_children_with(self);
        }
    }
    let mut reads = Reads(Vec::new());
    module.visit_with(&mut reads);
    reads.0
}

/// The targets of `/// <reference path="..." />` and `/// <reference
/// types="..." />` lines in `source`. A path is returned as a relative
/// specifier, a types name as the package it names. Comments are not in the
/// AST, so the leading `///` lines are read from the text.
pub fn reference_directives(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in source.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("///") else {
            // Directives are only honoured before the first statement.
            if line.starts_with("//") || line.starts_with("/*") || line.starts_with('*') {
                continue;
            }
            break;
        };
        let Some(tag) = rest.trim().strip_prefix("<reference") else {
            continue;
        };
        for (attribute, relative) in [("path", true), ("types", false)] {
            let Some(at) = tag.find(&format!("{attribute}=")) else {
                continue;
            };
            let value = &tag[at + attribute.len() + 1..];
            let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
                continue;
            };
            let Some(end) = value[1..].find(quote) else {
                continue;
            };
            let target = &value[1..1 + end];
            if relative && !target.starts_with('.') {
                found.push(format!("./{target}"));
            } else {
                found.push(target.to_string());
            }
        }
    }
    found
}

/// The specifiers `module` names in `import`, `export ... from` and
/// `export * from`, in source order.
fn static_specifiers(module: &Module) -> Vec<String> {
    use swc_ecma_ast::{ModuleDecl, ModuleItem};
    module
        .body
        .iter()
        .filter_map(|item| match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                Some(import.src.value.to_string_lossy().into_owned())
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(export)) => export
                .src
                .as_ref()
                .map(|src| src.value.to_string_lossy().into_owned()),
            ModuleItem::ModuleDecl(ModuleDecl::ExportAll(export)) => {
                Some(export.src.value.to_string_lossy().into_owned())
            }
            _ => None,
        })
        .collect()
}

/// Parse a JavaScript or TypeScript file into an AST
pub fn parse_file(
    file_path: &Path,
    source_map: &Lrc<SourceMap>,
    handler: &Handler,
) -> Option<Module> {
    let file_content = read_source(file_path)?;
    parse_source(
        file_path,
        FileName::Real(file_path.to_path_buf()),
        file_content,
        source_map,
        handler,
    )
}

/// The text [`parse_file`] parses `file_path` from, as read from disk (a BOM
/// included), or `None` for a file that is not script or cannot be read.
pub fn read_source(file_path: &Path) -> Option<String> {
    // A data module an import or `require` reaches (`./ranges.json`,
    // `../package.json`) is not script: handing it to the ES parser fails on
    // its first key and prints a parse-error block for a file no pass reads
    // (carrick#2300). The extension list is the one the walk and the
    // resolvers use, so they cannot disagree about a file.
    if !crate::file_finder::has_source_extension(file_path) {
        return None;
    }
    match fs::read_to_string(file_path) {
        Ok(content) => Some(content),
        Err(e) => {
            warn!("Error reading file {}: {}", file_path.display(), e);
            None
        }
    }
}

/// [`parse_file`] over text already in hand: a copy of `file_path` that is
/// not the one on disk, such as the one a commit holds (carrick#2285). The
/// path decides the syntax, as it does for a file read from disk; `name` is
/// what the source map files the text under, so two copies of one path stay
/// apart.
pub fn parse_source(
    file_path: &Path,
    name: FileName,
    file_content: String,
    source_map: &Lrc<SourceMap>,
    handler: &Handler,
) -> Option<Module> {
    let (syntax, is_typescript) = syntax_for_path(file_path);

    // Create source file in the source map
    let source_file = source_map.new_source_file(Lrc::new(name), file_content);

    // Create lexer and parser
    let lexer = Lexer::new(
        syntax,
        Default::default(),
        StringInput::from(&*source_file),
        None,
    );
    let mut parser = Parser::new_from(lexer);

    // Parse module
    for e in parser.take_errors() {
        e.into_diagnostic(handler).emit();
    }

    match parser.parse_module() {
        Ok(mut module) => {
            GLOBALS.set(&Globals::new(), || {
                let unresolved_mark = Mark::new();
                let top_level_mark = Mark::new();
                let mut pass = resolver(unresolved_mark, top_level_mark, is_typescript);
                module.visit_mut_with(&mut pass);
            });

            Some(module)
        }
        Err(e) => {
            e.into_diagnostic(handler).emit();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reference_directives_name_paths_and_packages_before_the_first_statement() {
        let source = "// header\n/// <reference path=\"g.d.ts\" />\n/// <reference types='node' />\nimport x from './x';\n/// <reference path=\"late.d.ts\" />\n";
        assert_eq!(
            super::reference_directives(source),
            vec!["./g.d.ts", "node"]
        );
    }

    use super::parse_file;
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };
    use swc_ecma_ast::*;
    use swc_ecma_visit::{Visit, VisitWith};

    #[derive(Default)]
    struct BindingIdCollector {
        global_var_i: Option<Id>,
        arrow_param_i: Option<Id>,
    }

    impl Visit for BindingIdCollector {
        fn visit_var_declarator(&mut self, var_decl: &VarDeclarator) {
            if self.global_var_i.is_none()
                && let Pat::Ident(binding) = &var_decl.name
                && binding.id.sym.as_ref() == "i"
            {
                self.global_var_i = Some(binding.id.to_id());
            }

            var_decl.visit_children_with(self);
        }

        fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
            if self.arrow_param_i.is_none() {
                for param in &arrow.params {
                    if let Pat::Ident(binding) = param
                        && binding.id.sym.as_ref() == "i"
                    {
                        self.arrow_param_i = Some(binding.id.to_id());
                        break;
                    }
                }
            }

            arrow.visit_children_with(self);
        }
    }

    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // carrick#2300: a data module reached by an import or a `require` is not
    // parsed as script, so no parse-error block is printed for it.
    #[test]
    fn a_data_module_is_not_parsed_as_script_and_prints_nothing() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp_dir.path().join("ranges.json"),
            r#"{"low": 1, "high": 9}"#,
        )
        .expect("write json");
        std::fs::write(
            tmp_dir.path().join("package.json"),
            r#"{"version": "1.2.3"}"#,
        )
        .expect("write package");
        std::fs::write(
            tmp_dir.path().join("main.ts"),
            "import ranges from './ranges.json' with { type: 'json' };\nconst pkg = require('./package.json');\nexport const top = ranges.high;\n",
        )
        .expect("write source");

        let cm: Lrc<SourceMap> = Default::default();
        let captured = Captured::default();
        let handler = Handler::with_emitter_writer(Box::new(captured.clone()), Some(cm.clone()));

        assert!(parse_file(&tmp_dir.path().join("ranges.json"), &cm, &handler).is_none());
        assert!(parse_file(&tmp_dir.path().join("package.json"), &cm, &handler).is_none());
        assert!(
            parse_file(&tmp_dir.path().join("main.ts"), &cm, &handler).is_some(),
            "the source file that imports them still parses"
        );
        let printed = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(printed.is_empty(), "no parse-error block: {printed}");
        assert!(!handler.has_errors());
    }

    #[test]
    fn test_resolver_produces_unique_ids_across_scopes() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");

        std::fs::write(
            &file_path,
            r#"
const i = "/global";
const routes = ["/a", "/b"]; 

routes.forEach((i) => {
  app.get(i, handler);
});
"#,
        )
        .expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));

        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

        let mut collector = BindingIdCollector::default();
        module.visit_with(&mut collector);

        let global_var_i = collector.global_var_i.expect("found global var i");
        let arrow_param_i = collector.arrow_param_i.expect("found arrow param i");

        assert_ne!(
            global_var_i, arrow_param_i,
            "expected distinct (sym, SyntaxContext) ids for same symbol bound in different scopes"
        );
    }
}
