//! A method decorated through a decorator factory of the service
//! (carrick#3688 C1; the stop half of #3781).
//!
//! `@Handles("x")` where `Handles` is a function of the service whose body
//! applies a labelled decorator export (`compose(OnMessage(event))`) states
//! no row: what the factory does with the name is not read. The factory's
//! body already holds the library site the site pass saved as an unread
//! site; this module only finds which methods call a factory and where its
//! body is, and [`crate::unread_sites::of_own_decorators`] joins the two.
//! It reads no body a second time. A decorator over no labelled export
//! saves nothing at its method.

use std::path::PathBuf;

use super::{RequestSummaryInputs, Site};

/// A decorator on a method that is a call of a bare name no library export
/// answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnDecorator {
    /// The name called.
    pub callee: String,
    /// The decorator's call.
    pub call: Site,
    /// The line of the method's name, where a row of the method is stated.
    pub line: u32,
}

/// One such decorator, with where its factory's body is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnDecoratorAt {
    /// The file the method is in, as walked.
    pub file: PathBuf,
    pub decorator: OwnDecorator,
    /// The file the factory is in, as walked, and its span there
    /// (`span_start`, `span_end`).
    pub factory_file: PathBuf,
    pub factory: (u32, u32),
}

/// Every method decorator of the service that calls one of its own
/// functions, in file, then source order. The function is the one the name
/// is declared as in the file, or the one an import of it reaches.
pub(crate) fn of(inputs: &RequestSummaryInputs) -> Vec<OwnDecoratorAt> {
    let mut found: Vec<OwnDecoratorAt> = Vec::new();
    for (file, ir) in &inputs.files {
        for function in ir.functions.values() {
            for decorator in &function.own_decorators {
                let local = ir
                    .function_spans
                    .get(&decorator.callee)
                    .map(|span| (file.clone(), *span));
                let reached = || {
                    let binding = inputs.bindings.get(file, &decorator.callee)?;
                    let crate::call_graph::ImportedBinding::Binding { file, name } = binding else {
                        return None;
                    };
                    let span = inputs.files.get(file)?.function_spans.get(name)?;
                    Some((file.clone(), *span))
                };
                let Some((factory_file, factory)) = local.or_else(reached) else {
                    continue;
                };
                found.push(OwnDecoratorAt {
                    file: file.clone(),
                    decorator: decorator.clone(),
                    factory_file,
                    factory,
                });
            }
        }
    }
    found.sort_by(|a, b| {
        (&a.file, a.decorator.call.span_start).cmp(&(&b.file, b.decorator.call.span_start))
    });
    found
}
