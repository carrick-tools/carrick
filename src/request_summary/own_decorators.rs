//! A method decorated through a decorator factory of the service
//! (carrick#3688 C1; carrick#3781).
//!
//! `@Handles("x")` where `Handles` is a function of the service whose body
//! applies a labelled decorator export (`compose(OnMessage(event))`). The
//! decorator's call is a caller record of the factory ([`OwnDecorator`]):
//! both caller tables read it as a module-level caller, so the library
//! reader restates the factory's labelled call as the decorator the method
//! gets and reads its name through what the method's decorator passes
//! ([`super::library_sites`]). Where no row is stated at the method,
//! [`crate::unread_sites::of_own_decorators`] joins this list with the site
//! the factory's body saved. It reads no body a second time. A decorator
//! over no labelled export saves nothing at its method.

use std::path::PathBuf;

use super::row_calls::Reach;
use super::{CallIr, RequestSummaryInputs};

/// A decorator on a method that is a call of a bare name no library export
/// answers to.
#[derive(Debug, Clone)]
pub(crate) struct OwnDecorator {
    /// The name called.
    pub callee: String,
    /// The decorator's call as a caller record: its site, its arguments,
    /// and the line of the method's name (`name_line`), where a row of the
    /// method is stated.
    pub(super) call: CallIr,
}

impl OwnDecorator {
    /// The decorator's call: its span and line.
    pub(crate) fn site(&self) -> &super::Site {
        &self.call.site
    }

    /// The line of the decorated method's name.
    pub(crate) fn line(&self) -> u32 {
        self.call.name_line
    }
}

/// One such decorator, with the function its call reaches.
#[derive(Debug, Clone)]
pub(crate) struct OwnDecoratorAt {
    /// The file the method is in, as walked.
    pub file: PathBuf,
    pub decorator: OwnDecorator,
    /// The file the factory is in, as walked, and its span there
    /// (`span_start`, `span_end`).
    pub factory_file: PathBuf,
    pub factory: (u32, u32),
}

/// Every method decorator of the service whose call reaches one of its own
/// module-scope functions, as [`RequestSummaryInputs::reach`] reads the
/// call (compiler rows first), in file, then source order.
pub(crate) fn of(inputs: &RequestSummaryInputs) -> Vec<OwnDecoratorAt> {
    let mut found: Vec<OwnDecoratorAt> = Vec::new();
    for (file, ir) in &inputs.files {
        for function in ir.functions.values() {
            for decorator in &function.own_decorators {
                let site = decorator.site();
                let Reach::Function(factory_file, factory_key) =
                    inputs.reach(file, site.lo, site.hi)
                else {
                    continue;
                };
                let Some(factory) = inputs
                    .files
                    .get(&factory_file)
                    .and_then(|factory_ir| factory_ir.function_spans.get(&factory_key))
                    .copied()
                else {
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
        (&a.file, a.decorator.site().span_start).cmp(&(&b.file, b.decorator.site().span_start))
    });
    found
}
