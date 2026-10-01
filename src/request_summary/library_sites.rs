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
//! - **Class fields** (carrick#1665): a `this.<field>` receiver is produced
//!   by today's constructor-only rule, tagged [`Holder::Field`], and contested
//!   with [`Contest::ClassField`] until that ticket's rule replaces it.
//! - **Value flow** (carrick#1562): [`literal_text`] and
//!   [`super::Reader::library_receiver`] are the only two places a name or a
//!   receiver is read. Name builders, names passed as parameters, imported
//!   constants, own-module factories and injected clients extend those two.
//! - **In-repo packages** (carrick#1666): a call the call graph resolves to a
//!   function of this service is that function's, and is no site here, which
//!   is where a workspace package's calls go today.
//!
//! The rules are in `docs/reference/client-semantics.md`, "Message roles".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use swc_common::{Span, Spanned};
use swc_ecma_ast::*;

use super::{
    ClientBinding, FnIr, LinkedClients, Reader, RequestSummaryInputs, Scope, Site, field_receiver,
    member_prop, prop_name,
};
use crate::binding_scope::ident_key;
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
    /// A class field (`this.<field>`), by today's constructor-only rule. Not
    /// read for message roles until carrick#1665.
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
    /// A class-field receiver, read once carrick#1665 lands.
    ClassField,
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
        if let SiteReceiver::Instance(maker) = &self.receiver
            && maker.holder == Holder::Field
        {
            return Some(Contest::ClassField);
        }
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
            let Some((client, declared_in)) = self.clients.client_in_scope(self.file, &site.client)
            else {
                continue;
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
                    holder: match &site.client {
                        ClientBinding::Own(_) if site.field => Holder::Field,
                        ClientBinding::Own(_) => Holder::Local,
                        ClientBinding::Module(_) | ClientBinding::Imported { .. } => Holder::Module,
                    },
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
    pub(super) client: ClientBinding,
    /// The receiver is a class field (`this.<field>`).
    pub(super) field: bool,
    pub(super) path: Vec<String>,
    pub(super) member: Option<String>,
    pub(super) args: Vec<SiteArg>,
}

/// The receiver a library site names: the binding, whether it is a class
/// field, the sub-object hops, the member, and the span of the member's name
/// (or the callee's).
type LibraryReceiver = (ClientBinding, bool, Vec<String>, Option<String>, Span);

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
        let (client, field, path, member, named) = self.library_receiver(callee, scope)?;
        Some(LibrarySiteIr {
            site: self.site(span),
            op_line: self.line(named),
            form,
            client,
            field,
            path,
            member,
            args: args.iter().map(|arg| site_arg(&arg.expr, scope)).collect(),
        })
    }

    /// The client a library site's callee names, by the same scope rules as
    /// an HTTP client's ([`Scope::call_binding`]): `client(…)`,
    /// `client.member(…)`, `client.a.b.member(…)`, and the same on a class
    /// field. Every hop is a plain name; anything computed is no site.
    ///
    /// The one place a library receiver is identified: value flow
    /// (carrick#1562) extends it, never a second resolver.
    fn library_receiver(&self, callee: &Expr, scope: &Scope<'_>) -> Option<LibraryReceiver> {
        let callee = unwrap_expression(callee);
        let outer = match callee {
            Expr::Ident(ident) => {
                return Some((
                    scope.call_binding(ident)?,
                    false,
                    Vec::new(),
                    None,
                    ident.span,
                ));
            }
            Expr::Member(outer) => outer,
            _ => return None,
        };
        if let Expr::This(_) = &*outer.obj {
            let client = field_receiver(outer, scope)?.clone();
            return Some((
                ClientBinding::Own(client),
                true,
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
                    return Some((scope.call_binding(ident)?, false, path, Some(member), named));
                }
                Expr::Member(inner) if matches!(&*inner.obj, Expr::This(_)) => {
                    path.reverse();
                    let client = field_receiver(inner, scope)?.clone();
                    return Some((ClientBinding::Own(client), true, path, Some(member), named));
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
             const TOPICS = { paid: \"order.paid\" };\n\
             export function a(ORDER_CREATED: string) { bus.publish(ORDER_CREATED, {}); }\n\
             export function b() { bus.publish(ORDER_CREATED, {}); }\n\
             export function c(flag: boolean) {\n\
             \x20 if (flag) {\n\
             \x20   const ORDER_CREATED = \"order.other\";\n\
             \x20   bus.publish(ORDER_CREATED, {});\n\
             \x20 }\n\
             }\n\
             export function d() { const local = `${ORDER_CREATED}.v2`; bus.publish(local, {}); }\n\
             export function e() { bus.publish(TOPICS.paid, {}); }\n\
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

    /// Who holds an instance: a function's own `const` is read, and a class
    /// field waits for carrick#1665.
    #[test]
    fn a_local_instance_is_read_and_a_class_field_waits_for_its_own_rule() {
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
        assert_eq!(maker(field).holder, Holder::Field);
        assert_eq!(field.contest(on_wire), Some(Contest::ClassField));
    }
}
