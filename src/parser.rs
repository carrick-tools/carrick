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
/// - **TypeScript or ES**, from `.ts`/`.tsx` against everything else.
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
        Some("ts") => (
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
    /// Every module specifier `file` imports from, in source order.
    ///
    /// Type-only imports are included: a package imported for its types alone
    /// is still imported, and a shared types package is exactly the member a
    /// dependents list exists to name.
    pub fn import_specifiers(&mut self, file: &Path) -> Vec<String> {
        use swc_ecma_ast::{ModuleDecl, ModuleItem};
        let Some(module) = parse_file(file, &self.source_map, &self.handler) else {
            return Vec::new();
        };
        module
            .body
            .iter()
            .filter_map(|item| match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                    Some(import.src.value.to_string())
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(export)) => {
                    export.src.as_ref().map(|src| src.value.to_string())
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportAll(export)) => {
                    Some(export.src.value.to_string())
                }
                _ => None,
            })
            .collect()
    }
}

/// Parse a JavaScript or TypeScript file into an AST
pub fn parse_file(
    file_path: &Path,
    source_map: &Lrc<SourceMap>,
    handler: &Handler,
) -> Option<Module> {
    let (syntax, is_typescript) = syntax_for_path(file_path);

    // Read file content
    let file_content = match fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(e) => {
            warn!("Error reading file {}: {}", file_path.display(), e);
            return None;
        }
    };

    // Create source file in the source map
    let source_file = source_map.new_source_file(
        // Create an Lrc wrapped FileName
        Lrc::new(FileName::Real(file_path.to_path_buf())),
        file_content,
    );

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
