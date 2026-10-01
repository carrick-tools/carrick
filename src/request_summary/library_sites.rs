//! The receiver core for message roles (carrick#1661, built from the
//! carrick#1616 slice): every call a service makes through a package export,
//! or through an instance one of the export's makers made, read from the
//! syntax alone, with what the call hands over.
//!
//! It is role-neutral. It says which receiver a call is made through
//! ([`LibrarySite::receiver_id`], the contract's receiver ids), which member,
//! through which sub-object hops, with which literal arguments, on which line,
//! and whether anything else the service does with the receiver could change
//! what the call means ([`LibrarySite::contest`]). Which receivers and members
//! a package's verified claims cover, and what row a covered call states, are
//! the claims' and the row writers' to say.
//!
//! The receiver is identified exactly as an HTTP library client is
//! ([`super::Scope::call_binding`], [`super::LinkedClients`]): a binding
//! imported from a package, or a binding a maker call on one built and never
//! reassigned, with every module's uses of a module-scope instance merged.
//! What differs for message roles:
//!
//! - **Module-level calls are read.** A definition is written where a module
//!   is loaded (`export const t = task({ id, run })`), so the calls outside
//!   every function are read too ([`super::FileIr::module_level`]). HTTP rows
//!   are composed from the functions alone, as before.
//! - **Makers of every form**: `export.member(…)`, `export(…)`, `new
//!   export(…)` and `new export.member(…)`, with whatever they are handed.
//!   Constructing the export is what a `new` maker does, so it contests
//!   nothing here; HTTP keeps its contest.
//! - **Literals are read by scope** ([`literal_text`]): a string or a template
//!   written at the call, or an identifier the resolver says names a constant
//!   (module or function scope) that holds one. A parameter, a block's own
//!   binding of the same name, an import and a member of a constant object are
//!   not literals here.
//! - **The line is the member name's**, so a chain written over several lines
//!   states its row where the member is named.
//! - **Sub-object hops** (`client.tasks.trigger(…)`) are a member path, not a
//!   member read that contests the client.
//! - **Class fields are read wherever the class writes them** (carrick#1665,
//!   [`field_receivers`]): a field every write in its class sets to an
//!   instance of one maker, handed the same arguments, is a receiver in each
//!   instance member, written in a method or not. Anything else that may set
//!   it (another value, an accessor, a decorator, a parameter property, a
//!   related class of the file, `this` handed to a call) leaves it no
//!   receiver, and its uses are its class's and its related classes' alone,
//!   so a field of the same name in another class of the file is another
//!   field. HTTP keeps its constructor-only rule.
//! - **The contest set.** A hand-off, a write, a member read that is not
//!   called, a spread, a member called by a key the source does not state, a
//!   namespace import of the module that holds an instance, and loading that
//!   module any other way contest the receiver ([`LibrarySite::contested`]),
//!   and a module the scan cannot follow turns imported reading off, as for
//!   HTTP. Every member called or constructed through the receiver is kept
//!   ([`LibrarySite::uses`]); the caller classifies each one against the
//!   package's surface, and only a member that can change a name, a prefix or
//!   a base, or one the surface does not list, contests. HTTP's rule, "any
//!   call outside the verified surface", stays HTTP's.
//!
//! Seams left for later tickets:
//!
//! - **Value flow** (carrick#1562): [`literal_text`],
//!   [`super::Reader::library_receiver`] and
//!   [`super::Reader::written_instance`] are the only three places a name, a
//!   receiver or what a class field is set to is read. Name builders, names
//!   passed as parameters, imported constants, own-module factories (a field
//!   set by `createRedisClient(…)` or `this.#connect()`) and injected clients
//!   extend those three.
//! - **In-repo packages** (carrick#1666): a call the call graph resolves to a
//!   function of this service is that function's, and is no site here, which
//!   is where a workspace package's calls go today.
//!
//! The rules are in `docs/reference/client-semantics.md`, "Message roles".

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use swc_common::{Span, Spanned};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use super::{
    BindingUse, BindingUses, ClassFields, ClientBinding, ClientRef, FileIr, FnIr, LinkedClients,
    ModuleScope, Reader, RequestSummaryInputs, Scope, Site, member_prop, prop_name, this_field,
};
use crate::binding_scope::{BindingKey, ident_key, pat_key};
use crate::graphql_document_sites::unwrap_expression;

/// Whether a maker or a call site calls its callee (`m(…)`) or constructs it
/// (`new m(…)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MakerForm {
    Call,
    New,
}

/// One argument, as a library claim's slot reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiteArg {
    /// The argument's text, when it is a literal ([`literal_text`]).
    pub text: Option<String>,
    /// The argument's keys, when it is an object literal.
    pub object: Option<SiteObject>,
    /// The argument is a function expression written at the call.
    pub function: bool,
}

/// An object literal's keys, each with its literal text when the value is
/// one ([`literal_text`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiteObject {
    /// Key -> its literal text, or `None` when the value is not one (a method
    /// or a getter included). A key written before a spread or a computed key
    /// is not here: what follows may overwrite it.
    pub fields: BTreeMap<String, Option<String>>,
    /// A spread or a computed key is in the object, so it may hold keys the
    /// source does not state.
    pub open: bool,
}

/// A member called or constructed through a receiver binding: `client.m(…)`
/// is `(Call, [], m)`, `client.tasks.trigger(…)` is `(Call, [tasks],
/// trigger)`, `client(…)` is `(Call, [], None)`, `new client.Worker(…)` is
/// `(New, [], Worker)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemberUse {
    pub form: MakerForm,
    pub path: Vec<String>,
    pub member: Option<String>,
}

/// What holds an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Holder {
    /// A `const` of a function body, or a callback's captured one.
    Local,
    /// A module-scope `const`, or an anonymous `export default <maker>`,
    /// read in its own module or in one that imports it (carrick#1568).
    Module,
    /// A class field (`this.<field>`) every write in its class sets to one
    /// maker's instance, read in the class's instance members
    /// (carrick#1665, [`field_receivers`]).
    Field,
}

/// The maker call that built an instance, read where it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteMaker {
    pub form: MakerForm,
    /// The export's member the maker is (`None`: the export itself).
    pub member: Option<String>,
    /// What the maker was handed, read in the declaring module's scope.
    pub args: Vec<SiteArg>,
    /// The module that declares the instance.
    pub file: PathBuf,
    /// The maker call's span start (SWC numbering) and line: the definition a
    /// name the maker binds resolves to.
    pub span_start: u32,
    pub line: u32,
    pub holder: Holder,
}

impl SiteMaker {
    /// The contract's receiver id for instances this maker builds:
    /// `instance:<member>`, `instance:()`, `instance:new` or
    /// `instance:new:<member>`.
    pub fn receiver_id(&self) -> String {
        match (self.form, self.member.as_deref()) {
            (MakerForm::Call, Some(member)) => format!("instance:{member}"),
            (MakerForm::Call, None) => "instance:()".to_string(),
            (MakerForm::New, None) => "instance:new".to_string(),
            (MakerForm::New, Some(member)) => format!("instance:new:{member}"),
        }
    }
}

/// The receiver a site's call is made through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteReceiver {
    /// The package export itself.
    Export,
    /// An instance one of the export's makers built.
    Instance(SiteMaker),
}

/// Which receivers a claim acts on (the contract's `on`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum On {
    Export,
    Instance,
    Both,
}

/// The receiver, member path and member an op claim names (the contract's
/// `on`, `of`, `path` and `member`).
#[derive(Debug, Clone, Copy)]
pub struct Selector<'a> {
    pub on: On,
    /// The maker whose instances the op acts on, named by its member. `None`:
    /// every instance.
    pub of: Option<&'a str>,
    pub path: &'a [String],
    pub member: Option<&'a str>,
}

/// What a package's surface says one member use is, as the caller
/// classifies it for [`LibrarySite::contest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberWire {
    /// A claimed maker, scope or op.
    OnWire,
    /// A member the surface lists as sending and receiving nothing.
    OffWire,
    /// A member that can change a name, a prefix or a base.
    ChangesName,
    /// A member the surface does not list.
    Unlisted,
}

/// Why a message-role reading of a site states nothing whatever the claims
/// say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contest {
    /// The receiver is used other than to call or construct through it, in
    /// any module that reaches it ([`LibrarySite::contested`]).
    Used,
    /// A member use the caller classified as able to change a name, or as
    /// unlisted, on the receiver `on` (the site's, or `export` for the export
    /// an instance was made from).
    Member {
        on: String,
        used: MemberUse,
        wire: MemberWire,
    },
}

/// One call or construction made through a package export or an instance of
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySite {
    pub file: PathBuf,
    /// The call's span, in SWC numbering ([`crate::swc_scanner::SWC_SPAN_BASE`]).
    pub span_start: u32,
    pub span_end: u32,
    /// The line of the member the call names, or of the callee: where a row
    /// it supports is stated.
    pub line: u32,
    pub form: MakerForm,
    /// The import specifier, as written (a package or one of its subpaths).
    pub specifier: String,
    /// `"default"` or the named export the binding imports.
    pub export: String,
    pub receiver: SiteReceiver,
    /// The sub-object hops between the receiver and the member.
    pub path: Vec<String>,
    /// The member called (`None`: the receiver itself).
    pub member: Option<String>,
    pub args: Vec<SiteArg>,
    /// The receiver is used, in a module that reaches it, in a way that may
    /// change it ([`Contest::Used`]).
    pub contested: bool,
    /// Every member called or constructed through the receiver, in every
    /// module that reaches it.
    pub uses: BTreeSet<MemberUse>,
    /// For an instance: every member called or constructed through the
    /// export's binding in the module that made it.
    pub export_uses: BTreeSet<MemberUse>,
}

impl LibrarySite {
    /// The contract's receiver id: `export`, or the instance's maker's
    /// ([`SiteMaker::receiver_id`]).
    pub fn receiver_id(&self) -> String {
        match &self.receiver {
            SiteReceiver::Export => "export".to_string(),
            SiteReceiver::Instance(maker) => maker.receiver_id(),
        }
    }

    /// The package the specifier names ([`package_name`]).
    pub fn package(&self) -> &str {
        package_name(&self.specifier)
    }

    /// Whether this site calls the export's maker of `form` on `member`
    /// (`None`: the export itself): where a definition is written.
    pub fn makes(&self, form: MakerForm, member: Option<&str>) -> bool {
        self.receiver == SiteReceiver::Export
            && self.form == form
            && self.path.is_empty()
            && self.member.as_deref() == member
    }

    /// Whether an op claim with `selector` acts on this site's call. An op is
    /// a call, never a construction.
    pub fn selected_by(&self, selector: &Selector<'_>) -> bool {
        let receiver = match (&self.receiver, selector.on) {
            (SiteReceiver::Export, On::Export | On::Both) => true,
            (SiteReceiver::Instance(maker), On::Instance | On::Both) => selector
                .of
                .is_none_or(|of| maker.member.as_deref() == Some(of)),
            _ => false,
        };
        receiver
            && self.form == MakerForm::Call
            && self.path == selector.path
            && self.member.as_deref() == selector.member
    }

    /// The literal argument `arg` holds, or its key `key`.
    pub fn literal(&self, arg: usize, key: Option<&str>) -> Option<&str> {
        arg_literal(&self.args, arg, key)
    }

    /// Whether the call supplies argument `arg`, or its key `key`.
    pub fn supplies(&self, arg: usize, key: Option<&str>) -> bool {
        arg_supplied(&self.args, arg, key)
    }

    /// Why a message-role reading of this site must state nothing, whatever
    /// the claims say: `None` when nothing contests it.
    ///
    /// `classify` says what one member use is on a receiver (`receiver`: the
    /// site's id, or `export` for the export an instance was made from). A use
    /// that can change a name, a prefix or a base contests, and so does one
    /// the package's surface does not list: nothing says it cannot.
    pub fn contest(&self, classify: impl Fn(&str, &MemberUse) -> MemberWire) -> Option<Contest> {
        if self.contested {
            return Some(Contest::Used);
        }
        let receiver = self.receiver_id();
        let uses = self
            .uses
            .iter()
            .map(|used| (receiver.as_str(), used))
            .chain(self.export_uses.iter().map(|used| ("export", used)));
        for (on, used) in uses {
            let wire = classify(on, used);
            if matches!(wire, MemberWire::ChangesName | MemberWire::Unlisted) {
                return Some(Contest::Member {
                    on: on.to_string(),
                    used: used.clone(),
                    wire,
                });
            }
        }
        None
    }
}

/// Every library site of a service, in file and span order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibrarySites {
    pub sites: Vec<LibrarySite>,
}

impl LibrarySites {
    /// Every package a site is made through, with the specifiers the service
    /// imports it by: what a library store is asked about. A bare specifier
    /// is any that is not relative, so a path alias (`@/lib/api`) is here
    /// too; which are installed packages is for the asker to read from
    /// `node_modules`.
    pub fn packages(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut packages: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for site in &self.sites {
            packages
                .entry(site.package().to_string())
                .or_default()
                .insert(site.specifier.clone());
        }
        packages
    }
}

/// The package a bare specifier names: `@scope/name` of `@scope/name/sub`,
/// `name` of `name/sub`. A runtime module (`node:events`) is its own.
pub fn package_name(specifier: &str) -> &str {
    let mut ends = specifier.match_indices('/').map(|(index, _)| index);
    let end = if specifier.starts_with('@') {
        ends.nth(1)
    } else {
        ends.next()
    };
    end.map_or(specifier, |end| &specifier[..end])
}

/// Every call and construction the service makes through a package export or
/// an instance of one ([`LibrarySite`]).
pub fn library_sites(inputs: &RequestSummaryInputs) -> LibrarySites {
    let clients = LinkedClients::link(&inputs.files, &inputs.bindings);
    let mut sites = Vec::new();
    let mut paths: Vec<&PathBuf> = inputs.files.keys().collect();
    paths.sort();
    for path in paths {
        let file = &inputs.files[path];
        let mut keys: Vec<&String> = file.functions.keys().collect();
        keys.sort();
        let reader = SiteReader {
            file: path,
            ir: file,
            clients: &clients,
            inputs,
        };
        for key in keys {
            reader.collect(&file.functions[key], &mut sites);
        }
        reader.collect(&file.module_level, &mut sites);
    }
    sites.sort_by(|a, b| {
        (&a.file, a.span_start, a.form, a.span_end).cmp(&(
            &b.file,
            b.span_start,
            b.form,
            b.span_end,
        ))
    });
    LibrarySites { sites }
}

struct SiteReader<'a> {
    file: &'a Path,
    ir: &'a FileIr,
    clients: &'a LinkedClients,
    inputs: &'a RequestSummaryInputs,
}

impl SiteReader<'_> {
    fn collect(&self, ir: &FnIr, out: &mut Vec<LibrarySite>) {
        for site in &ir.library {
            // A call the call graph resolves to a function of this service is
            // that function's, whatever its receiver's name.
            if site.form == MakerForm::Call
                && self.inputs.sites.target(self.file, site.site.lo).is_some()
            {
                continue;
            }
            let (client, declared_in, holder) = match &site.binding {
                SiteBinding::Client(binding) => {
                    let Some((client, declared_in)) =
                        self.clients.client_in_scope(self.file, binding)
                    else {
                        continue;
                    };
                    let holder = match &**binding {
                        ClientBinding::Own(_) => Holder::Local,
                        ClientBinding::Module(_) | ClientBinding::Imported { .. } => Holder::Module,
                    };
                    (client, declared_in, holder)
                }
                SiteBinding::Field { class, field } => {
                    let Some(client) = self.ir.field_receivers.get(&(*class, field.clone())) else {
                        continue;
                    };
                    (client, self.file, Holder::Field)
                }
            };
            // So is an instance such a call made: what it holds is that
            // function's return, not a package's.
            if let Some(instance) = &client.instance
                && instance.form == MakerForm::Call
                && self
                    .inputs
                    .sites
                    .target(declared_in, instance.site.lo)
                    .is_some()
            {
                continue;
            }
            let receiver = match &client.instance {
                None => SiteReceiver::Export,
                Some(instance) => SiteReceiver::Instance(SiteMaker {
                    form: instance.form,
                    member: instance.member.clone(),
                    args: instance.args.clone(),
                    file: declared_in.to_path_buf(),
                    span_start: instance.site.span_start,
                    line: instance.site.line,
                    holder,
                }),
            };
            out.push(LibrarySite {
                file: self.file.to_path_buf(),
                span_start: site.site.span_start,
                span_end: site.site.span_end,
                line: site.op_line,
                form: site.form,
                specifier: client.package.clone(),
                export: client.export.clone(),
                receiver,
                path: site.path.clone(),
                member: site.member.clone(),
                args: site.args.clone(),
                contested: client.contested_message,
                uses: client.member_uses.clone(),
                export_uses: client.export_uses.clone(),
            });
        }
        for nested in ir.nested.iter().chain(&ir.detached) {
            self.collect(nested, out);
        }
    }
}

/// A call or construction made through a library client, as one function
/// body (or the module's own level) writes it.
#[derive(Debug, Clone)]
pub(super) struct LibrarySiteIr {
    pub(super) site: Site,
    /// The line of the member named, or of the callee.
    pub(super) op_line: u32,
    pub(super) form: MakerForm,
    pub(super) binding: SiteBinding,
    pub(super) path: Vec<String>,
    pub(super) member: Option<String>,
    pub(super) args: Vec<SiteArg>,
}

/// What a library site's receiver is read through.
#[derive(Debug, Clone)]
pub(super) enum SiteBinding {
    /// A binding, by the scope rules an HTTP client's is
    /// ([`Scope::call_binding`]).
    Client(Box<ClientBinding>),
    /// `this.<field>` in an instance member of a class, named by the class's
    /// span start: what it holds is settled once every member of the class
    /// is read ([`field_receivers`]).
    Field { class: u32, field: String },
}

/// The receiver a library site names: the binding, the sub-object hops, the
/// member, and the span of the member's name (or the callee's).
type LibraryReceiver = (SiteBinding, Vec<String>, Option<String>, Span);

impl Reader<'_> {
    /// The library site a call or a `new` writes, when its callee names a
    /// library client. A spread argument moves every position after it, and
    /// the claims say what a position means, so such a call is no site.
    pub(super) fn library_site(
        &self,
        span: Span,
        form: MakerForm,
        callee: &Expr,
        args: &[ExprOrSpread],
        scope: &Scope<'_>,
    ) -> Option<LibrarySiteIr> {
        if args.iter().any(|arg| arg.spread.is_some()) {
            return None;
        }
        let (binding, path, member, named) = self.library_receiver(callee, scope)?;
        Some(LibrarySiteIr {
            site: self.site(span),
            op_line: self.line(named),
            form,
            binding,
            path,
            member,
            args: args.iter().map(|arg| site_arg(&arg.expr, scope)).collect(),
        })
    }

    /// The client a library site's callee names, by the same scope rules as
    /// an HTTP client's ([`Scope::call_binding`]): `client(…)`,
    /// `client.member(…)`, `client.a.b.member(…)`, and the same on a class
    /// field ([`field_binding`]). Every hop is a plain name; anything
    /// computed is no site.
    ///
    /// The one place a library receiver is identified: value flow
    /// (carrick#1562) extends it, never a second resolver.
    fn library_receiver(&self, callee: &Expr, scope: &Scope<'_>) -> Option<LibraryReceiver> {
        let callee = unwrap_expression(callee);
        let outer = match callee {
            Expr::Ident(ident) => {
                return Some((
                    SiteBinding::Client(Box::new(scope.call_binding(ident)?)),
                    Vec::new(),
                    None,
                    ident.span,
                ));
            }
            Expr::Member(outer) => outer,
            _ => return None,
        };
        if let Expr::This(_) = &*outer.obj {
            return Some((
                field_binding(outer, scope)?,
                Vec::new(),
                None,
                outer.prop.span(),
            ));
        }
        let member = member_prop(outer)?;
        let named = outer.prop.span();
        let mut path: Vec<String> = Vec::new();
        let mut obj = unwrap_expression(&outer.obj);
        loop {
            match obj {
                Expr::Ident(ident) => {
                    path.reverse();
                    let binding = SiteBinding::Client(Box::new(scope.call_binding(ident)?));
                    return Some((binding, path, Some(member), named));
                }
                Expr::Member(inner) if matches!(&*inner.obj, Expr::This(_)) => {
                    path.reverse();
                    return Some((field_binding(inner, scope)?, path, Some(member), named));
                }
                Expr::Member(inner) => {
                    path.push(member_prop(inner)?);
                    obj = unwrap_expression(&inner.obj);
                }
                _ => return None,
            }
        }
    }

    pub(super) fn line(&self, span: Span) -> u32 {
        self.source_map.lookup_char_pos(span.lo).line as u32
    }

    /// The instance a write to a class field sets, when the source states
    /// one: a maker call on a package export ([`Reader::factory_call`]).
    ///
    /// The one place what a field is set to is read: value flow
    /// (carrick#1562) extends it to an own-module factory
    /// (`this.redis = createRedisClient(…)`) and an injected client.
    fn written_instance(&self, expr: &Expr, scope: &Scope<'_>) -> Option<ClientRef> {
        self.factory_call(expr, scope)
    }

    /// The maker write `this.<field> = <maker>(…)` an assignment is, in an
    /// instance member of a class (where the class's field table is in
    /// scope, so `this` is the instance). Only `=` sets the field to the
    /// value written.
    pub(super) fn field_write(
        &self,
        assign: &AssignExpr,
        scope: &Scope<'_>,
    ) -> Option<FieldWriteIr> {
        if scope.fields.is_none() || assign.op != AssignOp::Assign {
            return None;
        }
        let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left else {
            return None;
        };
        if !is_this(&member.obj) {
            return None;
        }
        Some(FieldWriteIr {
            field: this_field(member)?,
            at: assign.span.lo.0,
            client: self.written_instance(&assign.right, scope)?,
        })
    }

    /// The maker writes a class's instance property initialisers and its
    /// constructor make, each read in its own scope: an initialiser in the
    /// module's, the constructor's statements in the constructor's.
    pub(super) fn initialiser_and_constructor_writes(
        &self,
        class: &Class,
        fields: &ClassFields,
        module: &ModuleScope,
    ) -> Vec<FieldWriteIr> {
        let mut writes = Vec::new();
        for member in &class.body {
            let (field, init, at) = match member {
                ClassMember::ClassProp(prop) if !prop.is_static => {
                    (prop_name(&prop.key), prop.value.as_deref(), prop.span.lo.0)
                }
                ClassMember::PrivateProp(prop) if !prop.is_static => (
                    Some(format!("#{}", prop.key.name)),
                    prop.value.as_deref(),
                    prop.span.lo.0,
                ),
                ClassMember::Constructor(ctor) => {
                    if let Some(body) = &ctor.body {
                        let mut scope = Scope::module(module);
                        scope.params = ctor.params.iter().map(constructor_param).collect();
                        scope.fields = Some(fields);
                        let mut ir = FnIr::default();
                        self.body(&body.stmts, &mut scope, &mut ir);
                        take_field_writes(&mut ir, &mut writes);
                    }
                    continue;
                }
                _ => continue,
            };
            if let (Some(field), Some(init)) = (field, init)
                && let Some(client) = self.written_instance(init, &Scope::module(module))
            {
                writes.push(FieldWriteIr { field, at, client });
            }
        }
        writes
    }
}

/// The binding a constructor parameter introduces: a parameter property's
/// is its name.
fn constructor_param(param: &ParamOrTsParamProp) -> Option<BindingKey> {
    match param {
        ParamOrTsParamProp::Param(param) => pat_key(&param.pat),
        ParamOrTsParamProp::TsParamProp(prop) => match &prop.param {
            TsParamPropParam::Ident(ident) => Some(ident_key(&ident.id)),
            TsParamPropParam::Assign(assign) => pat_key(&assign.left),
        },
    }
}

/// The field `this.<field>` names, in an instance member of a class, where
/// the class's field table is in scope. In a static member, or a function
/// that binds its own `this`, no field of the class is.
fn field_binding(member: &MemberExpr, scope: &Scope<'_>) -> Option<SiteBinding> {
    Some(SiteBinding::Field {
        class: scope.fields?.class,
        field: this_field(member)?,
    })
}

/// A write `this.<field> = <maker>(…)` an instance member makes
/// (carrick#1665): the field, where the assignment starts (discovery
/// numbering, the key [`ClassThis::writes`] holds it by), and the instance it
/// sets.
#[derive(Debug, Clone)]
pub(super) struct FieldWriteIr {
    field: String,
    at: u32,
    client: ClientRef,
}

/// Move every maker write out of a function body and those written inside it.
pub(super) fn take_field_writes(ir: &mut FnIr, out: &mut Vec<FieldWriteIr>) {
    out.append(&mut ir.field_writes);
    for nested in ir.nested.iter_mut().chain(ir.detached.iter_mut()) {
        take_field_writes(nested, out);
    }
}

/// The fields of the class bound to `class` that hold one maker's instance
/// (carrick#1665), given the maker writes its members make (`writes`).
///
/// A field is one when every write to it in the class is one of `writes`
/// and they all build an instance of one maker of one export, handed the
/// same arguments; when nothing else may set it (an accessor of its name, a
/// decorator, a parameter property); when no class of the file that it
/// extends, or that extends it, writes or declares it; and when neither the
/// class nor those let `this` escape ([`ClassThis::escapes`]). The instance
/// is the first write's, used as the class and those classes use the field.
pub(super) fn field_receivers(
    class: &BindingKey,
    classes: &HashMap<BindingKey, ClassThis>,
    writes: Vec<FieldWriteIr>,
) -> HashMap<String, ClientRef> {
    let mut out = HashMap::new();
    let Some(own) = classes.get(class) else {
        return out;
    };
    let related = relatives(class, classes);
    if own.escapes || related.iter().any(|class| class.escapes) {
        return out;
    }
    let mut by_field: BTreeMap<String, Vec<FieldWriteIr>> = BTreeMap::new();
    for write in writes {
        by_field.entry(write.field.clone()).or_default().push(write);
    }
    for (field, mut made) in by_field {
        let made_at: BTreeSet<u32> = made.iter().map(|write| write.at).collect();
        if own.writes.get(&field) != Some(&made_at)
            || own.opaque.contains(&field)
            || related.iter().any(|class| class.sets(&field))
        {
            continue;
        }
        made.sort_by_key(|write| write.at);
        let first = &made[0].client;
        if !made.iter().all(|write| same_maker(&write.client, first)) {
            continue;
        }
        let key = format!("this.{field}");
        let mut used = BindingUse::default();
        for class in std::iter::once(own).chain(related.iter().copied()) {
            if let Some(uses) = class.uses.get(&key) {
                used.merge(uses);
            }
        }
        out.insert(field, first.clone().used_as(&used));
    }
    out
}

/// Whether two instances are one maker's, handed the same arguments.
fn same_maker(a: &ClientRef, b: &ClientRef) -> bool {
    a.package == b.package
        && a.export == b.export
        && match (&a.instance, &b.instance) {
            (Some(a), Some(b)) => a.form == b.form && a.member == b.member && a.args == b.args,
            _ => false,
        }
}

/// Every class of the file `class` extends, directly or through another, and
/// every class that extends it so: on an instance of either, a field of one
/// is a field of the other.
fn relatives<'c>(
    class: &BindingKey,
    classes: &'c HashMap<BindingKey, ClassThis>,
) -> Vec<&'c ClassThis> {
    let mut related: BTreeSet<&BindingKey> = ancestors(class, classes);
    for other in classes.keys() {
        if ancestors(other, classes).contains(class) {
            related.insert(other);
        }
    }
    related.remove(class);
    related.iter().filter_map(|key| classes.get(*key)).collect()
}

/// The bindings `class` extends, directly or through a class of the file.
fn ancestors<'c>(
    class: &BindingKey,
    classes: &'c HashMap<BindingKey, ClassThis>,
) -> BTreeSet<&'c BindingKey> {
    let mut seen: BTreeSet<&BindingKey> = BTreeSet::new();
    let mut next: Vec<&BindingKey> = classes
        .get(class)
        .map(|facts| facts.extends.iter().collect())
        .unwrap_or_default();
    while let Some(base) = next.pop() {
        if seen.insert(base)
            && let Some(facts) = classes.get(base)
        {
            next.extend(facts.extends.iter());
        }
    }
    seen
}

/// What one class does with `this` (carrick#1665): every write to a field
/// of it, the fields something other than a write may set, whether `this`
/// escapes, and how each field is used. Read over the whole class body,
/// static members and the functions written in it included, so a write
/// where `this` may not be the instance counts as one that may be.
#[derive(Debug, Default)]
pub(super) struct ClassThis {
    /// The class it extends, by the binding the name resolves to
    /// ([`crate::binding_scope`]).
    extends: BTreeSet<BindingKey>,
    /// Each field written, with where every write starts (discovery
    /// numbering): an assignment, an update, a `delete`, a destructuring
    /// target, or an instance property's initialiser (the property's start).
    writes: HashMap<String, BTreeSet<u32>>,
    /// Instance properties declared with no value: a class that extends
    /// another and declares one again replaces the field.
    declared: HashSet<String>,
    /// Fields something other than a write in the source may set: a
    /// decorated property, a constructor's parameter property, an accessor
    /// of that name.
    opaque: HashSet<String>,
    /// `this` is handed to a call or a construction, aliased, destructured,
    /// spread, or read or written by a computed key, or a member is named by
    /// one: any field may hold anything, and a use of one may be unseen.
    /// Returned, or put in an object or an array, it leaves the class as
    /// `new` hands it out, which is not followed for any holder.
    escapes: bool,
    /// How the class uses each field, keyed `this.<field>` ([`BindingUses`]).
    uses: HashMap<String, BindingUse>,
}

impl ClassThis {
    /// What `class` does with `this`.
    fn of(class: &Class) -> Self {
        let mut facts = ClassThis::default();
        if let Some(Expr::Ident(base)) = class.super_class.as_deref().map(unwrap_expression) {
            facts.extends.insert(ident_key(base));
        }
        for member in &class.body {
            match member {
                ClassMember::ClassProp(prop) if !prop.is_static => match prop_name(&prop.key) {
                    Some(name) => facts.property(name, &prop.decorators, &prop.value, prop.span),
                    None => facts.escapes = true,
                },
                ClassMember::PrivateProp(prop) if !prop.is_static => {
                    let name = format!("#{}", prop.key.name);
                    facts.property(name, &prop.decorators, &prop.value, prop.span);
                }
                ClassMember::Constructor(ctor) => {
                    for param in &ctor.params {
                        if let ParamOrTsParamProp::TsParamProp(prop) = param {
                            let name = match &prop.param {
                                TsParamPropParam::Ident(ident) => Some(&ident.id),
                                TsParamPropParam::Assign(assign) => {
                                    assign.left.as_ident().map(|binding| &binding.id)
                                }
                            };
                            match name {
                                Some(ident) => {
                                    facts.opaque.insert(ident.sym.to_string());
                                }
                                None => facts.escapes = true,
                            }
                        }
                    }
                }
                ClassMember::Method(method)
                    if !method.is_static
                        && matches!(method.kind, MethodKind::Getter | MethodKind::Setter) =>
                {
                    match prop_name(&method.key) {
                        Some(name) => {
                            facts.opaque.insert(name);
                        }
                        None => facts.escapes = true,
                    }
                }
                ClassMember::PrivateMethod(method)
                    if !method.is_static
                        && matches!(method.kind, MethodKind::Getter | MethodKind::Setter) =>
                {
                    facts.opaque.insert(format!("#{}", method.key.name));
                }
                _ => {}
            }
        }
        class.body.visit_with(&mut ThisWalker { facts: &mut facts });
        let mut uses = BindingUses::default();
        class.body.visit_with(&mut uses);
        facts.uses = uses
            .uses
            .into_iter()
            .filter(|(key, _)| key.starts_with("this."))
            .collect();
        facts
    }

    /// An instance property: decorated (set by whatever the decorator
    /// installs), initialised (a write where the property starts), or
    /// declared.
    fn property(
        &mut self,
        name: String,
        decorators: &[Decorator],
        value: &Option<Box<Expr>>,
        span: Span,
    ) {
        if !decorators.is_empty() {
            self.opaque.insert(name);
        } else if value.is_some() {
            self.write(name, span.lo.0);
        } else {
            self.declared.insert(name);
        }
    }

    fn write(&mut self, field: String, at: u32) {
        self.writes.entry(field).or_default().insert(at);
    }

    /// Whether the class writes or declares `field`, or may set it unseen.
    fn sets(&self, field: &str) -> bool {
        self.writes.contains_key(field)
            || self.declared.contains(field)
            || self.opaque.contains(field)
    }
}

/// Every class this file names, by its binding ([`ClassThis`]): a class of
/// the same name in another scope is another class.
pub(super) fn class_this(module: &Module) -> HashMap<BindingKey, ClassThis> {
    #[derive(Default)]
    struct Classes {
        found: HashMap<BindingKey, ClassThis>,
    }
    impl Visit for Classes {
        fn visit_class_decl(&mut self, decl: &ClassDecl) {
            self.found
                .insert(ident_key(&decl.ident), ClassThis::of(&decl.class));
            decl.visit_children_with(self);
        }
        fn visit_class_expr(&mut self, expr: &ClassExpr) {
            if let Some(ident) = &expr.ident {
                self.found
                    .insert(ident_key(ident), ClassThis::of(&expr.class));
            }
            expr.visit_children_with(self);
        }
    }
    let mut classes = Classes::default();
    module.visit_with(&mut classes);
    classes.found
}

/// The writes to fields of `this` in a class body, and the ways `this` leaves
/// it other than as a value ([`ClassThis::escapes`]).
struct ThisWalker<'f> {
    facts: &'f mut ClassThis,
}

impl ThisWalker<'_> {
    /// A write at `at` to the member `target` of `this`. One by a computed
    /// key may write any field.
    fn write(&mut self, target: &MemberExpr, at: u32) {
        match this_field(target) {
            Some(field) => self.facts.write(field, at),
            None => self.facts.escapes = true,
        }
        if let MemberProp::Computed(key) = &target.prop {
            key.expr.visit_with(self);
        }
    }
}

impl Visit for ThisWalker<'_> {
    /// `this` anywhere no visit below keeps it from: handed on.
    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::This(_) = expr {
            self.facts.escapes = true;
            return;
        }
        expr.visit_children_with(self);
    }

    /// `this.field` names a field; `this[key]` may name any.
    fn visit_member_expr(&mut self, member: &MemberExpr) {
        if !is_this(&member.obj) {
            member.visit_children_with(self);
            return;
        }
        if let MemberProp::Computed(key) = &member.prop {
            self.facts.escapes = true;
            key.expr.visit_with(self);
        }
    }

    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        match &assign.left {
            AssignTarget::Simple(target) => match simple_member(target) {
                Some(member) if is_this(&member.obj) => self.write(member, assign.span.lo.0),
                _ => match target {
                    SimpleAssignTarget::SuperProp(sup) => match &sup.prop {
                        SuperProp::Ident(ident) => {
                            self.facts.write(ident.sym.to_string(), assign.span.lo.0);
                        }
                        SuperProp::Computed(_) => self.facts.escapes = true,
                    },
                    other => other.visit_with(self),
                },
            },
            AssignTarget::Pat(pat) => pat.visit_with(self),
        }
        assign.right.visit_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        match unwrap_expression(&update.arg) {
            Expr::Member(member) if is_this(&member.obj) => self.write(member, update.span.lo.0),
            _ => update.visit_children_with(self),
        }
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        match (unary.op, unwrap_expression(&unary.arg)) {
            (UnaryOp::Delete, Expr::Member(member)) if is_this(&member.obj) => {
                self.write(member, unary.span.lo.0);
            }
            _ => unary.visit_children_with(self),
        }
    }

    /// A member of `this` as a destructuring or loop target is written.
    fn visit_pat(&mut self, pat: &Pat) {
        if let Pat::Expr(expr) = pat
            && let Expr::Member(member) = unwrap_expression(expr)
            && is_this(&member.obj)
        {
            self.write(member, expr.span().lo.0);
            return;
        }
        pat.visit_children_with(self);
    }

    /// `this` handed to a call may be written by it (`Object.assign(this,
    /// options)`), unless it binds one of the class's own methods to it.
    fn visit_call_expr(&mut self, call: &CallExpr) {
        call.callee.visit_with(self);
        let binds_own = matches!(&call.callee, Callee::Expr(callee) if binds_own_method(callee));
        for (index, arg) in call.args.iter().enumerate() {
            if index == 0 && binds_own && arg.spread.is_none() && is_this(&arg.expr) {
                continue;
            }
            arg.visit_with(self);
        }
    }

    /// `new this()` constructs, as `new` on the class does.
    fn visit_new_expr(&mut self, new: &NewExpr) {
        if !is_this(&new.callee) {
            new.callee.visit_with(self);
        }
        new.args.visit_with(self);
    }

    fn visit_bin_expr(&mut self, bin: &BinExpr) {
        let compares = matches!(
            bin.op,
            BinaryOp::InstanceOf
                | BinaryOp::EqEq
                | BinaryOp::NotEq
                | BinaryOp::EqEqEq
                | BinaryOp::NotEqEq
        );
        for side in [&bin.left, &bin.right] {
            if !(compares && is_this(side)) {
                side.visit_with(self);
            }
        }
    }

    /// Returned, or put in an object or an array, `this` leaves the class
    /// as a value, as `new` hands it out.
    fn visit_return_stmt(&mut self, ret: &ReturnStmt) {
        match &ret.arg {
            Some(arg) if is_this(arg) => {}
            _ => ret.visit_children_with(self),
        }
    }

    fn visit_key_value_prop(&mut self, prop: &KeyValueProp) {
        prop.key.visit_with(self);
        if !is_this(&prop.value) {
            prop.value.visit_with(self);
        }
    }

    fn visit_array_lit(&mut self, array: &ArrayLit) {
        for element in array.elems.iter().flatten() {
            if element.spread.is_none() && is_this(&element.expr) {
                continue;
            }
            element.visit_with(self);
        }
    }
}

fn is_this(expr: &Expr) -> bool {
    matches!(unwrap_expression(expr), Expr::This(_))
}

/// `this.method.bind(…)`, `.call(…)` or `.apply(…)`: one of the class's own
/// methods, whose writes are the class's.
fn binds_own_method(callee: &Expr) -> bool {
    let Expr::Member(outer) = unwrap_expression(callee) else {
        return false;
    };
    matches!(
        member_prop(outer).as_deref(),
        Some("bind" | "call" | "apply")
    ) && matches!(unwrap_expression(&outer.obj), Expr::Member(inner) if is_this(&inner.obj))
}

/// The member an assignment target is, through any parentheses or type
/// assertion around it.
fn simple_member(target: &SimpleAssignTarget) -> Option<&MemberExpr> {
    let inner = match target {
        SimpleAssignTarget::Member(member) => return Some(member),
        SimpleAssignTarget::Paren(paren) => &paren.expr,
        SimpleAssignTarget::TsAs(cast) => &cast.expr,
        SimpleAssignTarget::TsSatisfies(cast) => &cast.expr,
        SimpleAssignTarget::TsNonNull(cast) => &cast.expr,
        SimpleAssignTarget::TsTypeAssertion(cast) => &cast.expr,
        _ => return None,
    };
    match unwrap_expression(inner) {
        Expr::Member(member) => Some(member),
        _ => None,
    }
}

/// One argument's reading ([`SiteArg`]), in `scope`.
pub(super) fn site_arg(expr: &Expr, scope: &Scope<'_>) -> SiteArg {
    let expr = unwrap_expression(expr);
    SiteArg {
        text: literal_text(expr, scope),
        object: match expr {
            Expr::Object(object) => Some(site_object(object, scope)),
            _ => None,
        },
        function: matches!(expr, Expr::Arrow(_) | Expr::Fn(_)),
    }
}

/// An object literal's keys ([`SiteObject`]). A spread or a computed key can
/// overwrite what came before it, so every earlier key is dropped and the
/// object is open; a key written after it is known.
fn site_object(object: &ObjectLit, scope: &Scope<'_>) -> SiteObject {
    let mut out = SiteObject::default();
    let open = |out: &mut SiteObject| {
        out.fields.clear();
        out.open = true;
    };
    for prop in &object.props {
        let prop = match prop {
            PropOrSpread::Spread(_) => {
                open(&mut out);
                continue;
            }
            PropOrSpread::Prop(prop) => prop,
        };
        match &**prop {
            Prop::KeyValue(kv) => match prop_name(&kv.key) {
                Some(key) => {
                    out.fields.insert(key, literal_text(&kv.value, scope));
                }
                None => open(&mut out),
            },
            Prop::Shorthand(ident) => {
                out.fields.insert(
                    ident.sym.to_string(),
                    literal_text(&Expr::Ident(ident.clone()), scope),
                );
            }
            Prop::Method(MethodProp { key, .. })
            | Prop::Getter(GetterProp { key, .. })
            | Prop::Setter(SetterProp { key, .. }) => match prop_name(key) {
                Some(key) => {
                    out.fields.insert(key, None);
                }
                None => open(&mut out),
            },
            Prop::Assign(_) => open(&mut out),
        }
    }
    out
}

/// The text `expr` is, when the source states it at compile time: a string,
/// a template whose every hole is one, a `+` of two, or an identifier the
/// resolver says names a binding [`Scope::text`] holds (a `const`, or a
/// function's own settled binding, whose initialiser is one). A parameter,
/// a binding a nested block declares, an import, a member of a constant
/// object, a call and any operator but `+` are not.
///
/// The one place a name is read: value flow (carrick#1562) extends it.
pub(super) fn literal_text(expr: &Expr, scope: &Scope<'_>) -> Option<String> {
    match unwrap_expression(expr) {
        Expr::Lit(Lit::Str(text)) => Some(text.value.to_string()),
        Expr::Tpl(tpl) => {
            let mut out = String::new();
            for (index, quasi) in tpl.quasis.iter().enumerate() {
                match &quasi.cooked {
                    Some(cooked) => out.push_str(cooked.as_ref()),
                    None => out.push_str(&quasi.raw),
                }
                if let Some(hole) = tpl.exprs.get(index) {
                    out.push_str(&literal_text(hole, scope)?);
                }
            }
            Some(out)
        }
        Expr::Bin(bin) if bin.op == BinaryOp::Add => {
            let left = literal_text(&bin.left, scope)?;
            let right = literal_text(&bin.right, scope)?;
            Some(left + right.as_str())
        }
        Expr::Ident(ident) => scope.text(ident).map(str::to_string),
        _ => None,
    }
}

impl Scope<'_> {
    /// The literal text the binding `ident` resolves to holds, by its key
    /// ([`crate::binding_scope`]): never another binding of the same name.
    fn text(&self, ident: &Ident) -> Option<&str> {
        if self.param_index(ident).is_some() {
            return None;
        }
        let key = ident_key(ident);
        self.texts
            .get(&key)
            .or_else(|| self.module.texts.get(&key))
            .map(String::as_str)
    }
}

fn arg_literal<'a>(args: &'a [SiteArg], arg: usize, key: Option<&str>) -> Option<&'a str> {
    let arg = args.get(arg)?;
    match key {
        None => arg.text.as_deref(),
        Some(key) => arg.object.as_ref()?.fields.get(key)?.as_deref(),
    }
}

fn arg_supplied(args: &[SiteArg], arg: usize, key: Option<&str>) -> bool {
    let Some(arg) = args.get(arg) else {
        return false;
    };
    match key {
        None => true,
        Some(key) => arg
            .object
            .as_ref()
            .is_some_and(|object| object.fields.contains_key(key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest declaring every package the tests import, so no specifier
    /// reads as one the scan cannot follow unless a test means it to.
    const MANIFEST: &str = "{ \"name\": \"service\", \"dependencies\": { \"@fixture/jobs\": \"^1.0.0\", \"@fixture/queue\": \"^2.0.0\", \"@fixture/bus\": \"^1.0.0\", \"fixture-bus\": \"^1.0.0\" } }\n";

    /// The library sites discovery reads in a service of `files`.
    fn sites_of(files: &[(&str, &str)]) -> Vec<LibrarySite> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("package.json"), MANIFEST).expect("write manifest");
        for (name, source) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&path, source).expect("write source");
        }
        let inputs = crate::engine::discover_request_inputs(dir.path());
        library_sites(&inputs).sites
    }

    /// The one site in `file` (a path suffix) at `line` whose member is
    /// `member`.
    fn site<'s>(
        sites: &'s [LibrarySite],
        file: &str,
        line: u32,
        member: Option<&str>,
    ) -> &'s LibrarySite {
        let found: Vec<&LibrarySite> = sites
            .iter()
            .filter(|site| {
                site.file.ends_with(file) && site.line == line && site.member.as_deref() == member
            })
            .collect();
        match found.as_slice() {
            [one] => one,
            _ => panic!("expected one site at {file}:{line} {member:?}, got {sites:#?}"),
        }
    }

    fn maker(site: &LibrarySite) -> &SiteMaker {
        match &site.receiver {
            SiteReceiver::Instance(maker) => maker,
            SiteReceiver::Export => panic!("expected an instance receiver: {site:#?}"),
        }
    }

    /// Every use reads as a claimed maker or op.
    fn on_wire(_: &str, _: &MemberUse) -> MemberWire {
        MemberWire::OnWire
    }

    /// A definition is written where its module is loaded, so a call at
    /// module level is a site, and an instance a definition made carries the
    /// maker and what it was handed into every module that imports it.
    #[test]
    fn a_definition_at_module_level_is_a_site_and_its_instance_carries_the_maker() {
        let sites = sites_of(&[
            (
                "src/tasks.ts",
                "import { task } from \"@fixture/jobs\";\n\
                 export const sendEmail = task({\n\
                 \x20 id: \"send-email\",\n\
                 \x20 run: async (payload: { to: string }) => {},\n\
                 });\n",
            ),
            (
                "src/api.ts",
                "import { sendEmail } from \"./tasks\";\n\
                 export async function signup(to: string) {\n\
                 \x20 await sendEmail.trigger({ to });\n\
                 }\n",
            ),
        ]);

        let definition = site(&sites, "src/tasks.ts", 2, None);
        assert!(definition.makes(MakerForm::Call, None));
        assert_eq!(definition.specifier, "@fixture/jobs");
        assert_eq!(definition.export, "task");
        assert_eq!(definition.literal(0, Some("id")), Some("send-email"));
        assert!(definition.supplies(0, Some("run")));
        assert_eq!(definition.literal(0, Some("run")), None);

        let trigger = site(&sites, "src/api.ts", 3, Some("trigger"));
        assert_eq!(trigger.receiver_id(), "instance:()");
        let made = maker(trigger);
        assert!(made.file.ends_with("src/tasks.ts"));
        assert_eq!(made.line, 2);
        assert_eq!(made.span_start, definition.span_start);
        assert_eq!(made.holder, Holder::Module);
        assert_eq!(
            made.args[0]
                .object
                .as_ref()
                .expect("an options object")
                .fields["id"]
                .as_deref(),
            Some("send-email")
        );
        assert_eq!(trigger.contest(on_wire), None);
    }

    /// `new` is a maker: the instance carries what the constructor was
    /// handed, and constructing the export contests nothing for a message
    /// role.
    #[test]
    fn a_new_maker_builds_an_instance_and_contests_nothing() {
        let sites = sites_of(&[(
            "src/mail.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             const emails = new Queue(\"emails\");\n\
             export async function welcome(to: string) {\n\
             \x20 await emails.add(\"welcome\", { to });\n\
             }\n",
        )]);

        let constructed = site(&sites, "src/mail.ts", 2, None);
        assert!(constructed.makes(MakerForm::New, None));
        assert_eq!(constructed.literal(0, None), Some("emails"));
        assert!(!constructed.contested);

        let add = site(&sites, "src/mail.ts", 4, Some("add"));
        assert_eq!(add.receiver_id(), "instance:new");
        assert_eq!(maker(add).args[0].text.as_deref(), Some("emails"));
        assert_eq!(add.literal(0, None), Some("welcome"));
        assert!(add.export_uses.contains(&MemberUse {
            form: MakerForm::New,
            path: Vec::new(),
            member: None,
        }));
        assert_eq!(add.contest(on_wire), None);
    }

    /// `client.tasks.trigger(…)` is a call through a sub-object: a member
    /// path, never a member read that takes the client away.
    #[test]
    fn a_call_through_a_sub_object_is_a_member_path_not_a_member_read() {
        let sites = sites_of(&[(
            "src/report.ts",
            "import { client } from \"@fixture/jobs\";\n\
             export async function nightly() {\n\
             \x20 await client.tasks.trigger(\"nightly-report\", { day: 1 });\n\
             }\n",
        )]);

        let trigger = site(&sites, "src/report.ts", 3, Some("trigger"));
        assert_eq!(trigger.receiver, SiteReceiver::Export);
        assert_eq!(trigger.path, vec!["tasks".to_string()]);
        assert!(!trigger.contested);
        assert!(trigger.uses.contains(&MemberUse {
            form: MakerForm::Call,
            path: vec!["tasks".to_string()],
            member: Some("trigger".to_string()),
        }));
        let path = ["tasks".to_string()];
        assert!(trigger.selected_by(&Selector {
            on: On::Export,
            of: None,
            path: &path,
            member: Some("trigger"),
        }));
        assert!(!trigger.selected_by(&Selector {
            on: On::Export,
            of: None,
            path: &[],
            member: Some("trigger"),
        }));
    }

    /// `on` picks the export or its instances, and `of` the instances of one
    /// maker, named by its member.
    #[test]
    fn selectors_pick_the_instances_of_the_maker_they_name() {
        let sites = sites_of(&[(
            "src/jobs.ts",
            "import { jobs } from \"@fixture/jobs\";\n\
             const nightly = jobs.task({ id: \"nightly\" });\n\
             const progress = jobs.stream({ id: \"progress\" });\n\
             export async function go() {\n\
             \x20 await nightly.trigger({});\n\
             \x20 await progress.append(\"tick\");\n\
             \x20 await jobs.trigger(\"nightly\", {});\n\
             }\n",
        )]);
        let select = |on: On, of: Option<&'static str>, member: &'static str| Selector {
            on,
            of,
            path: &[],
            member: Some(member),
        };

        let on_task = site(&sites, "src/jobs.ts", 5, Some("trigger"));
        assert_eq!(on_task.receiver_id(), "instance:task");
        assert!(on_task.selected_by(&select(On::Instance, Some("task"), "trigger")));
        assert!(!on_task.selected_by(&select(On::Instance, Some("stream"), "trigger")));
        assert!(on_task.selected_by(&select(On::Instance, None, "trigger")));
        assert!(on_task.selected_by(&select(On::Both, None, "trigger")));
        assert!(!on_task.selected_by(&select(On::Export, None, "trigger")));

        let on_stream = site(&sites, "src/jobs.ts", 6, Some("append"));
        assert_eq!(on_stream.receiver_id(), "instance:stream");
        assert!(!on_stream.selected_by(&select(On::Instance, Some("task"), "append")));

        let on_export = site(&sites, "src/jobs.ts", 7, Some("trigger"));
        assert!(on_export.selected_by(&select(On::Export, None, "trigger")));
        assert!(on_export.selected_by(&select(On::Both, Some("task"), "trigger")));
        assert!(!on_export.selected_by(&select(On::Instance, None, "trigger")));
    }

    /// A subpath specifier is kept as the service imports it, and names the
    /// package a store is asked about.
    #[test]
    fn a_subpath_specifier_is_kept_and_names_its_package() {
        let sites = LibrarySites {
            sites: sites_of(&[(
                "src/send.ts",
                "import { tasks } from \"@fixture/jobs/v3\";\n\
                 import bus from \"fixture-bus/client\";\n\
                 export function send() {\n\
                 \x20 tasks.trigger(\"a\", {});\n\
                 \x20 bus.emit(\"b\", {});\n\
                 }\n",
            )]),
        };

        let trigger = site(&sites.sites, "src/send.ts", 4, Some("trigger"));
        assert_eq!(trigger.specifier, "@fixture/jobs/v3");
        assert_eq!(trigger.package(), "@fixture/jobs");
        let emit = site(&sites.sites, "src/send.ts", 5, Some("emit"));
        assert_eq!(emit.export, "default");
        assert_eq!(emit.package(), "fixture-bus");
        assert_eq!(
            sites.packages(),
            BTreeMap::from([
                (
                    "@fixture/jobs".to_string(),
                    BTreeSet::from(["@fixture/jobs/v3".to_string()])
                ),
                (
                    "fixture-bus".to_string(),
                    BTreeSet::from(["fixture-bus/client".to_string()])
                ),
            ])
        );
        assert_eq!(package_name("@fixture/jobs"), "@fixture/jobs");
        assert_eq!(package_name("fixture-bus"), "fixture-bus");
        assert_eq!(package_name("node:events"), "node:events");
        assert_eq!(package_name("@fixture/jobs/v3/ai"), "@fixture/jobs");
    }

    /// A name is the binding the identifier resolves to: a module constant
    /// where the module's binding is meant, never where a parameter or a
    /// block's own binding of the name is, and a constant built from
    /// constants. A member of a constant object, and a binding assigned
    /// again, are not literals here.
    #[test]
    fn a_name_is_read_in_its_own_scope() {
        let sites = sites_of(&[(
            "src/orders.ts",
            "import { bus } from \"@fixture/bus\";\n\
             const ORDER_CREATED = \"order.created\";\n\
             const TOPICS = { shipped: \"order.shipped\" };\n\
             export function a(ORDER_CREATED: string) { bus.publish(ORDER_CREATED, {}); }\n\
             export function b() { bus.publish(ORDER_CREATED, {}); }\n\
             export function c(flag: boolean) {\n\
             \x20 if (flag) {\n\
             \x20   const ORDER_CREATED = \"order.other\";\n\
             \x20   bus.publish(ORDER_CREATED, {});\n\
             \x20 }\n\
             }\n\
             export function d() { const local = `${ORDER_CREATED}.v2`; bus.publish(local, {}); }\n\
             export function e() { bus.publish(TOPICS.shipped, {}); }\n\
             export function f() { let topic = \"order.x\"; topic = \"order.y\"; bus.publish(topic, {}); }\n\
             bus.publish(ORDER_CREATED + \".audit\", {});\n",
        )]);
        let name = |line: u32| {
            site(&sites, "src/orders.ts", line, Some("publish"))
                .literal(0, None)
                .map(str::to_string)
        };
        assert_eq!(name(4), None, "a parameter of the same name");
        assert_eq!(name(5).as_deref(), Some("order.created"));
        assert_eq!(name(9), None, "a block's own binding is never the module's");
        assert_eq!(name(12).as_deref(), Some("order.created.v2"));
        assert_eq!(
            name(13),
            None,
            "a member of a constant object (carrick#1562)"
        );
        assert_eq!(name(14), None, "a binding assigned again");
        assert_eq!(name(15).as_deref(), Some("order.created.audit"));
    }

    /// A chain written over several lines states its row on the member's
    /// line.
    #[test]
    fn the_line_is_the_member_name_s() {
        let sites = sites_of(&[(
            "src/report.ts",
            "import { client } from \"@fixture/jobs\";\n\
             export async function nightly() {\n\
             \x20 await client\n\
             \x20   .tasks\n\
             \x20   .trigger(\"nightly\", {});\n\
             }\n",
        )]);
        let trigger = site(&sites, "src/report.ts", 5, Some("trigger"));
        assert_eq!(trigger.path, vec!["tasks".to_string()]);
    }

    /// A member the surface lists as off the wire contests nothing; one it
    /// does not list, or one that can change a name, does. The export an
    /// instance was made from counts too.
    #[test]
    fn a_member_use_contests_by_what_the_surface_says_it_is() {
        let sites = sites_of(&[(
            "src/mail.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             Queue.setPrefix(\"staging\");\n\
             const emails = new Queue(\"emails\");\n\
             export async function welcome() { await emails.add(\"welcome\", {}); }\n\
             export async function stop() { await emails.close(); }\n",
        )]);
        let add = site(&sites, "src/mail.ts", 4, Some("add"));
        let close = MemberUse {
            form: MakerForm::Call,
            path: Vec::new(),
            member: Some("close".to_string()),
        };
        assert!(add.uses.contains(&close));
        let classify = |close_is: MemberWire, prefix_is: MemberWire| {
            move |on: &str, used: &MemberUse| match (on, used.member.as_deref()) {
                ("instance:new", Some("close")) => close_is,
                ("export", Some("setPrefix")) => prefix_is,
                _ => MemberWire::OnWire,
            }
        };
        assert_eq!(
            add.contest(classify(MemberWire::OffWire, MemberWire::OffWire)),
            None
        );
        assert_eq!(
            add.contest(classify(MemberWire::Unlisted, MemberWire::OffWire)),
            Some(Contest::Member {
                on: "instance:new".to_string(),
                used: close,
                wire: MemberWire::Unlisted,
            })
        );
        assert_eq!(
            add.contest(classify(MemberWire::OffWire, MemberWire::ChangesName)),
            Some(Contest::Member {
                on: "export".to_string(),
                used: MemberUse {
                    form: MakerForm::Call,
                    path: Vec::new(),
                    member: Some("setPrefix".to_string()),
                },
                wire: MemberWire::ChangesName,
            })
        );
    }

    /// The uses that still contest: a hand-off or a write in any module that
    /// reaches the instance, a member called by a key the source does not
    /// state, and a namespace import of its module used other than to call a
    /// binding it publishes.
    #[test]
    fn a_hand_off_a_write_a_computed_call_or_a_namespace_contests_the_instance() {
        let queue = "import { Queue } from \"@fixture/queue\";\n\
                     export const emails = new Queue(\"emails\");\n\
                     export async function welcome() { await emails.add(\"welcome\", {}); }\n";
        let control = sites_of(&[
            ("src/queue.ts", queue),
            (
                "src/user.ts",
                "import { emails } from \"./queue\";\n\
                 export async function later() { await emails.add(\"later\", {}); }\n",
            ),
        ]);
        assert_eq!(
            site(&control, "src/queue.ts", 3, Some("add")).contest(on_wire),
            None,
            "an importer that only calls through the instance contests nothing"
        );
        for (case, user) in [
            (
                "a hand-off in another module",
                "import { emails } from \"./queue\";\n\
                 import { register } from \"./registry\";\n\
                 register(emails);\n",
            ),
            (
                "a write in another module",
                "import { emails } from \"./queue\";\n\
                 (emails as any).prefix = \"staging\";\n",
            ),
            (
                "a computed call",
                "import { emails } from \"./queue\";\n\
                 export function call(name: string) { return (emails as any)[name](); }\n",
            ),
            (
                "a namespace call through a member",
                "import * as queue from \"./queue\";\n\
                 export function send() { return queue.emails.add(\"later\", {}); }\n",
            ),
        ] {
            let sites = sites_of(&[
                ("src/queue.ts", queue),
                ("src/user.ts", user),
                (
                    "src/registry.ts",
                    "export function register(value: unknown) {}\n",
                ),
            ]);
            let add = site(&sites, "src/queue.ts", 3, Some("add"));
            assert_eq!(add.contest(on_wire), Some(Contest::Used), "{case}");
        }
    }

    /// A module the scan cannot follow turns imported reading off, as it does
    /// for an HTTP client (carrick#1568): the importer reads nothing through
    /// the instance, and the declaring module still reads its own calls.
    #[test]
    fn a_module_the_scan_cannot_follow_turns_imported_reading_off() {
        let sites = sites_of(&[
            (
                "src/queue.ts",
                "import { Queue } from \"@fixture/queue\";\n\
                 export const emails = new Queue(\"emails\");\n\
                 export async function welcome() { await emails.add(\"welcome\", {}); }\n",
            ),
            (
                "src/user.ts",
                "import { emails } from \"./queue\";\n\
                 import { setup } from \"~/nowhere\";\n\
                 setup();\n\
                 export async function later() { await emails.add(\"later\", {}); }\n",
            ),
        ]);
        assert!(
            sites
                .iter()
                .all(|site| !(site.file.ends_with("src/user.ts") && site.line == 4)),
            "{sites:#?}"
        );
        site(&sites, "src/queue.ts", 3, Some("add"));
    }

    /// A key a later spread may overwrite holds nothing the source states,
    /// and a spread argument moves every position, so it is no site.
    #[test]
    fn a_key_a_later_spread_may_overwrite_is_not_read() {
        let sites = sites_of(&[(
            "src/tasks.ts",
            "import { task } from \"@fixture/jobs\";\n\
             import { bus } from \"@fixture/bus\";\n\
             const defaults = { queue: \"main\" };\n\
             export const before = task({ id: \"before\", ...defaults });\n\
             export const after = task({ ...defaults, id: \"after\" });\n\
             export function send(args: [string, unknown]) { bus.publish(...args); }\n",
        )]);
        assert_eq!(
            site(&sites, "src/tasks.ts", 4, None).literal(0, Some("id")),
            None
        );
        assert_eq!(
            site(&sites, "src/tasks.ts", 5, None).literal(0, Some("id")),
            Some("after")
        );
        assert!(
            sites.iter().all(|site| site.line != 6),
            "a spread argument is no site: {sites:#?}"
        );
    }

    /// A call the call graph resolves to a function of this service is that
    /// function's, and so is what it returns: a package of the repo's own
    /// (carrick#1666) is read through its source, never as a library.
    #[test]
    fn a_call_into_the_service_s_own_package_is_no_site() {
        let sites = sites_of(&[
            (
                "packages/jobs/package.json",
                "{ \"name\": \"@fixture/jobs-local\", \"main\": \"src/index.ts\" }\n",
            ),
            (
                "packages/jobs/src/index.ts",
                "export function task(options: { id: string }) {\n\
                 \x20 return { trigger: (payload: unknown) => options.id };\n\
                 }\n",
            ),
            (
                "src/tasks.ts",
                "import { task } from \"@fixture/jobs-local\";\n\
                 import { Queue } from \"@fixture/queue\";\n\
                 export const nightly = task({ id: \"nightly\" });\n\
                 export function run() { return nightly.trigger({}); }\n\
                 export const emails = new Queue(\"emails\");\n\
                 export function send() { return emails.add(\"welcome\", {}); }\n",
            ),
        ]);
        let in_tasks: Vec<(u32, Option<&str>)> = sites
            .iter()
            .filter(|site| site.file.ends_with("src/tasks.ts"))
            .map(|site| (site.line, site.member.as_deref()))
            .collect();
        assert_eq!(in_tasks, vec![(5, None), (6, Some("add"))], "{sites:#?}");
    }

    /// Who holds an instance: a function's own `const`, and a class field
    /// whose initialiser is the maker.
    #[test]
    fn a_local_instance_and_an_initialised_field_are_read() {
        let sites = sites_of(&[(
            "src/mail.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             export async function once() {\n\
             \x20 const q = new Queue(\"once\");\n\
             \x20 await q.add(\"ping\", {});\n\
             }\n\
             export class Mailer {\n\
             \x20 private q = new Queue(\"emails\");\n\
             \x20 send() { return this.q.add(\"welcome\", {}); }\n\
             }\n",
        )]);
        let local = site(&sites, "src/mail.ts", 4, Some("add"));
        assert_eq!(maker(local).holder, Holder::Local);
        assert_eq!(local.contest(on_wire), None);
        let field = site(&sites, "src/mail.ts", 8, Some("add"));
        assert_eq!(field.receiver_id(), "instance:new");
        assert_eq!(maker(field).holder, Holder::Field);
        assert_eq!(maker(field).line, 7);
        assert_eq!(maker(field).args[0].text.as_deref(), Some("emails"));
        assert_eq!(field.contest(on_wire), None);
    }

    /// The service the class-field tests read: `body` is a class (or
    /// classes) written after one import of each export.
    fn class_sites(body: &str) -> Vec<LibrarySite> {
        let source = format!(
            "import {{ Queue, Worker }} from \"@fixture/queue\";\n\
             import {{ register }} from \"./registry\";\n\
             {body}"
        );
        sites_of(&[
            ("src/mail.ts", source.as_str()),
            (
                "src/registry.ts",
                "export function register(value: unknown) {}\n",
            ),
        ])
    }

    /// The `add` site on `line` of a [`class_sites`] service, if there is one.
    fn add_at(sites: &[LibrarySite], line: u32) -> Option<&LibrarySite> {
        sites.iter().find(|site| {
            site.file.ends_with("src/mail.ts")
                && site.line == line
                && site.member.as_deref() == Some("add")
        })
    }

    /// A field is a receiver wherever its class writes it (carrick#1665):
    /// in a method, behind a guard, in the constructor and a method alike,
    /// so long as every write sets an instance of one maker with the same
    /// arguments. Each write's arguments are read in its own scope, and the
    /// instance is the first write's.
    #[test]
    fn a_field_every_write_sets_to_one_maker_is_a_receiver_in_any_method() {
        let sites = class_sites(
            "export class Starter {\n\
             \x20 private q?: Queue;\n\
             \x20 async start() {\n\
             \x20   const name = \"emails\";\n\
             \x20   this.q = new Queue(name);\n\
             \x20 }\n\
             \x20 async send(to: string) { await this.q.add(\"welcome\", { to }); }\n\
             }\n\
             export class Lazy {\n\
             \x20 private q: Queue | undefined;\n\
             \x20 reset() {\n\
             \x20   if (!this.q) {\n\
             \x20     this.q = new Queue(\"jobs\");\n\
             \x20   }\n\
             \x20 }\n\
             \x20 constructor() { this.q = new Queue(\"jobs\"); }\n\
             \x20 send() { return this.q.add(\"tick\", {}); }\n\
             }\n\
             export class Held {\n\
             \x20 #q = new Queue(\"held\");\n\
             \x20 send() { return this.#q.add(\"ping\", {}); }\n\
             }\n\
             export class Deferred {\n\
             \x20 private q?: Queue;\n\
             \x20 start() { setTimeout(() => { this.q = new Queue(\"later\"); }, 10); }\n\
             \x20 send() { return this.q.add(\"tock\", {}); }\n\
             }\n",
        );

        let in_method = add_at(&sites, 9).expect("a field written in a method");
        assert_eq!(in_method.receiver_id(), "instance:new");
        assert_eq!(maker(in_method).holder, Holder::Field);
        assert_eq!(maker(in_method).line, 7);
        assert_eq!(maker(in_method).args[0].text.as_deref(), Some("emails"));
        assert_eq!(in_method.literal(0, None), Some("welcome"));
        assert_eq!(in_method.contest(on_wire), None);

        let guarded = add_at(&sites, 19).expect("a field the class writes twice");
        assert_eq!(
            maker(guarded).line,
            15,
            "the first write's instance, whichever member it is in"
        );
        assert_eq!(maker(guarded).args[0].text.as_deref(), Some("jobs"));
        assert_eq!(
            guarded.contest(on_wire),
            None,
            "a truthiness test changes nothing"
        );

        let private = add_at(&sites, 23).expect("a private field");
        assert_eq!(maker(private).args[0].text.as_deref(), Some("held"));
        assert_eq!(private.contest(on_wire), None);

        let deferred = add_at(&sites, 28).expect("a field written in a callback");
        assert_eq!(maker(deferred).args[0].text.as_deref(), Some("later"));
    }

    /// A field is no receiver when anything but one maker's instance may be
    /// in it: another maker, the same maker handed other arguments, a
    /// parameter, a write by any operator but `=`, a write where `this` is
    /// not the instance read here, a related class's write or declaration, a
    /// decorator, an accessor, or `this` handed on, aliased, spread or
    /// written by a computed key. A static member reads no instance field.
    #[test]
    fn a_field_anything_else_may_write_is_no_receiver() {
        let control = class_sites(
            "export class Mailer {\n\
             \x20 private q: Queue;\n\
             \x20 start() { this.q = new Queue(\"emails\"); }\n\
             \x20 send() { return this.q.add(\"welcome\", {}); }\n\
             }\n",
        );
        assert!(add_at(&control, 6).is_some(), "control: {control:#?}");

        for (case, class) in [
            (
                "two makers",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 swap() { this.q = new Worker(\"emails\"); }\n\
                 }\n",
            ),
            (
                "one maker handed other arguments",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 swap() { this.q = new Queue(\"other\"); }\n\
                 }\n",
            ),
            (
                "a parameter",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 use(q: Queue) { this.q = q; }\n\
                 }\n",
            ),
            (
                "a parameter property",
                "export class Mailer {\n\
                 \x20 constructor(private q: Queue) {}\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "a logical assignment",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q ??= new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "a destructuring write",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 swap(next: Queue) { ({ q: this.q } = { q: next }); }\n\
                 }\n",
            ),
            (
                "an update",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 bump() { this.q++; }\n\
                 }\n",
            ),
            (
                "a property by a computed key",
                "const KEY = \"q\"; export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 [KEY] = new Worker(\"emails\");\n\
                 }\n",
            ),
            (
                "a subclass that hands this to a call",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n\
                 export class Special extends Mailer { init(options: object) { Object.assign(this, options); } }\n",
            ),
            (
                "a delete",
                "export class Mailer {\n\
                 \x20 private q?: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 stop() { delete this.q; }\n\
                 }\n",
            ),
            (
                "a write through a type assertion",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 swap(next: unknown) { (this.q as any) = next; }\n\
                 }\n",
            ),
            (
                "a write through super",
                "class Base {} export class Mailer extends Base {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 swap() { super.q = new Worker(\"emails\"); }\n\
                 }\n",
            ),
            (
                "a private setter of the same name",
                "export class Mailer {\n\
                 \x20 set #q(value: Queue) {}\n\
                 \x20 start() { this.#q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.#q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "a function expression's this",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { setTimeout(function () { this.q = new Queue(\"emails\"); }); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "a setter's write",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 set name(value: string) { this.q = new Queue(value); }\n\
                 }\n",
            ),
            (
                "a subclass that declares it again",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n\
                 export class Special extends Mailer { q = new Queue(\"special\"); }\n",
            ),
            (
                "a base class that writes it plainly",
                "class Base { reset() { this.q = undefined as any; } } export class Mailer extends Base {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "a base class that writes it",
                "class Base { reset() { (this as any).q = undefined; } } export class Mailer extends Base {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "a decorator",
                "export class Mailer {\n\
                 \x20 @inject() private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n\
                 function inject() { return (target: unknown, key: string) => {}; }\n",
            ),
            (
                "an accessor of the same name",
                "export class Mailer {\n\
                 \x20 set q(value: Queue) {}\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "this handed to a call",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start(options: object) { this.q = new Queue(\"emails\"); Object.assign(this, options); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "this aliased",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); const self = this; self.q.setName(\"x\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "this destructured",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); const { q } = this; q.setName(\"x\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "this spread",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); return { ...this }; }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "a write by a computed key",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start(key: string, value: unknown) { this.q = new Queue(\"emails\"); (this as any)[key] = value; }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n",
            ),
            (
                "a read by a computed key",
                "export class Mailer {\n\
                 \x20 private q: Queue;\n\
                 \x20 start() { this.q = new Queue(\"emails\"); }\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 \x20 pick(key: string) { return (this as any)[key].setName(\"x\"); }\n\
                 }\n",
            ),
        ] {
            let sites = class_sites(class);
            assert!(add_at(&sites, 6).is_none(), "{case}: {sites:#?}");
        }

        let statics = class_sites(
            "export class Mailer {\n\
             \x20 private q = new Queue(\"emails\");\n\
             \x20 static q = new Queue(\"static\");\n\
             \x20 send() { return this.q.add(\"welcome\", {}); }\n\
             \x20 static send() { return this.q.add(\"static\", {}); }\n\
             }\n",
        );
        assert_eq!(
            maker(add_at(&statics, 6).expect("the instance member's call")).args[0]
                .text
                .as_deref(),
            Some("emails")
        );
        assert!(
            add_at(&statics, 7).is_none(),
            "a static member reads no instance field: {statics:#?}"
        );
    }

    /// The instance may leave the class the way `new` hands it out: returned,
    /// or as a value in an object or an array, or bound to the class's own
    /// method; and a static member may construct one. A class that does no
    /// more than that still reads its field.
    #[test]
    fn this_handed_out_as_a_value_or_bound_keeps_the_field() {
        let sites = class_sites(
            "export class Mailer {\n\
             \x20 private q: Queue;\n\
             \x20 start() { this.q = new Queue(\"emails\"); return this; }\n\
             \x20 send() { return this.q.add(\"welcome\", { by: this, all: [this] }); }\n\
             \x20 listen(on: (f: () => void) => void) { on(this.stop.bind(this)); }\n\
             \x20 stop() { return this === undefined; }\n\
             \x20 static create() { return new this(); }\n\
             }\n",
        );
        let add = add_at(&sites, 6).expect("the field is read");
        assert_eq!(add.contest(on_wire), None);
    }

    /// A field's uses are its class's: a hand-off or a member read in the
    /// class, or in a class of the file that extends it, contests it, and
    /// the same field name in an unrelated class of the file is another
    /// field (the F1 finding of the carrick#1626 socket review). A class is
    /// its binding, so one of the same name in another scope is another
    /// class.
    #[test]
    fn a_field_s_uses_are_its_own_class_s() {
        let sites = class_sites(
            "export class Mailer {\n\
             \x20 private q = new Queue(\"emails\");\n\
             \x20 send() { return this.q.add(\"welcome\", {}); }\n\
             }\n\
             export class Jobs {\n\
             \x20 private q = new Worker(\"jobs\");\n\
             \x20 send() { register(this.q); return this.q.add(\"tick\", {}); }\n\
             }\n\
             export class Reports {\n\
             \x20 private q = new Queue(\"reports\");\n\
             \x20 send() { return this.q.add(\"nightly\", {}); }\n\
             }\n\
             export class Monthly extends Reports {\n\
             \x20 peek() { return this.q.defaults; }\n\
             }\n\
             export function make() { return class Mailer { send() { register(this); } }; }\n",
        );

        let mailer = add_at(&sites, 5).expect("Mailer's field");
        assert_eq!(mailer.export, "Queue");
        assert_eq!(maker(mailer).args[0].text.as_deref(), Some("emails"));
        assert_eq!(
            mailer.contest(on_wire),
            None,
            "another class's hand-off of its own field"
        );

        let jobs = add_at(&sites, 9).expect("Jobs' field");
        assert_eq!(jobs.export, "Worker");
        assert_eq!(maker(jobs).args[0].text.as_deref(), Some("jobs"));
        assert_eq!(jobs.contest(on_wire), Some(Contest::Used), "a hand-off");

        let reports = add_at(&sites, 13).expect("Reports' field");
        assert_eq!(
            reports.contest(on_wire),
            Some(Contest::Used),
            "a member read in a subclass"
        );
    }
}
