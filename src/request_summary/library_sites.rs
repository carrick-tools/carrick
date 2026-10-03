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
//!   (module or function scope) that holds one. A block's own binding of the
//!   same name is not; a parameter is a hole a caller fills, and an import or
//!   an entry of a constant object is read through the module's bindings
//!   (value flow, below).
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
//! - **The contest set.** A hand-off, a write, a member read used as a
//!   value, a spread, a member called by a key the source does not state, a
//!   namespace import of the module that holds an instance, and loading that
//!   module any other way contest the receiver ([`LibrarySite::contested`]),
//!   and a module the scan cannot follow turns imported reading off, as for
//!   HTTP. The receiver or a member read off it in a test position contests
//!   nothing (carrick#1665, carrick#1690, [`super::BindingUses::tested`]).
//!   Every member called or constructed through the receiver is kept
//!   ([`LibrarySite::uses`]); the caller classifies each one against the
//!   package's surface, and only a member that can change a name, a prefix or
//!   a base, or one the surface does not list, contests. HTTP's rule, "any
//!   call outside the verified surface", stays HTTP's.
//!
//! - **Value flow** (carrick#1562). What a holder is set to is read in one
//!   place ([`super::Reader::written_instance`]), a receiver in one
//!   ([`super::Reader::library_receiver`]) and a name in one
//!   ([`text_pieces`], under [`literal_text`]). Each reads through the
//!   service's own code:
//!   - an own factory's return ([`super::FnIr::returned`]), followed where
//!     the call graph resolves the call (`SiteReader::made`), so
//!     `this.queue = createQueue("emails")` holds the factory's maker's
//!     instance, handed `"emails"` ([`SiteMaker::factory`]). A factory that
//!     builds one of a few makers' instances on every path (a `let` set on
//!     each branch, a return per branch) holds the set of those makers
//!     (carrick#1689, [`SiteReceiver::Instance`]), and a site through it
//!     states a fact only when every maker reads the same
//!     ([`LibrarySite::fold`]). A getter that builds one instance into a
//!     module `let` on first use and returns it is followed the same way
//!     (carrick#1790, [`Reader::lazy_clients`]), and since every call holds
//!     that one object, every holder's uses are each site's
//!     ([`share_lazy_clients`]);
//!   - a name taken from a parameter, stated again at each call that fills
//!     it with text ([`LibrarySite::origin`]);
//!   - an entry of a constant object, an imported constant, and what a
//!     builder returns for its arguments, read once every module's uses are
//!     known ([`LinkedNames`]).
//!
//! Seams left for later tickets:
//!
//! - **In-repo packages** (carrick#1666): a call the call graph resolves to a
//!   function of this service is that function's, and is no site here, which
//!   is where a workspace package's calls go today.
//! - **A client handed in as a parameter** (carrick#1693) is read as
//!   nothing, and so are a set of makers of more than one export
//!   (carrick#1704) and a conditional of two makers' instances
//!   (carrick#1705).
//!
//! The rules are in `docs/reference/client-semantics.md`, "Message roles".

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use swc_common::{Span, Spanned};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use super::{
    BindingUse, BindingUses, ClassFields, ClientBinding, ClientInstance, ClientRef, FileIr, FnIr,
    LinkedClients, MadeBy, ModuleScope, OwnCallee, Reader, RequestSummaryInputs, Scope, Site,
    member_prop, prop_name, this_field,
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
    /// The argument's text, when it is a literal ([`literal_text`]). On a
    /// site a caller fills ([`LibrarySite::origin`]), only the text this call
    /// filled in.
    pub text: Option<String>,
    /// The argument's keys, when it is an object literal.
    pub object: Option<SiteObject>,
    /// The argument is a function expression written at the call.
    pub function: bool,
    /// Text with a parameter of the enclosing function in it
    /// ([`text_pieces`]): a hole each caller fills with its own argument
    /// (carrick#1562). `text` is `None` while a hole is open.
    pub(super) holes: Option<Vec<TextPiece>>,
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
    /// The keys whose value is text with a parameter in it, as
    /// [`SiteArg::holes`].
    pub(super) holes: BTreeMap<String, Vec<TextPiece>>,
}

impl SiteArg {
    /// Whether a parameter is in the argument's text, or in one of its
    /// keys' (carrick#1562).
    pub(super) fn has_holes(&self) -> bool {
        self.holes.is_some()
            || self
                .object
                .as_ref()
                .is_some_and(|object| !object.holes.is_empty())
    }

    /// This argument at a call passing `passed` to the function it is
    /// written in (carrick#1562): each hole filled with the text the call
    /// passes there, or left open with the call's own parameters in it, and
    /// holding nothing where the call passes anything else. An argument that
    /// is one parameter is what the call passes, object and all.
    ///
    /// With `keep`, the text the argument states itself stays (a maker's
    /// argument, read through a factory). Without it, only what this call
    /// filled in is text, so a site a caller fills never states again what
    /// the library call states itself. The flag says whether a hole became
    /// text at this call.
    pub(super) fn filled(&self, passed: &[SiteArg], keep: bool) -> (SiteArg, bool) {
        if let Some([TextPiece::Param(index)]) = self.holes.as_deref() {
            let Some(arg) = passed.get(*index) else {
                return (SiteArg::default(), false);
            };
            let filled = arg.text.is_some()
                || arg
                    .object
                    .as_ref()
                    .is_some_and(|object| object.fields.values().any(Option::is_some));
            return (arg.clone(), filled);
        }
        let mut filled = false;
        let mut fill = |pieces: &[TextPiece]| {
            let (text, holes) = split_text(fill_pieces(pieces, passed));
            filled |= text.is_some();
            (text, holes)
        };
        let (text, holes) = match &self.holes {
            Some(pieces) => fill(pieces),
            None => (self.text.clone().filter(|_| keep), None),
        };
        let object = self.object.as_ref().map(|object| {
            let mut out = SiteObject {
                fields: BTreeMap::new(),
                open: object.open,
                holes: BTreeMap::new(),
            };
            for (key, value) in &object.fields {
                let (text, holes) = match object.holes.get(key) {
                    Some(pieces) => fill(pieces),
                    None => (value.clone().filter(|_| keep), None),
                };
                if let Some(holes) = holes {
                    out.holes.insert(key.clone(), holes);
                }
                out.fields.insert(key.clone(), text);
            }
            out
        });
        (
            SiteArg {
                text,
                object,
                function: self.function,
                holes,
            },
            filled,
        )
    }
}

/// `pieces` with each parameter replaced by what `passed` holds at its
/// position: its text, or its own holes. `None` when the call passes
/// anything else there, or nothing.
fn fill_pieces(pieces: &[TextPiece], passed: &[SiteArg]) -> Option<Vec<TextPiece>> {
    let mut out = Vec::new();
    for piece in pieces {
        match piece {
            TextPiece::Param(index) => {
                let arg = passed.get(*index)?;
                match (&arg.text, &arg.holes) {
                    (Some(text), _) => push_pieces(&mut out, vec![TextPiece::Lit(text.clone())]),
                    (None, Some(holes)) => push_pieces(&mut out, holes.clone()),
                    (None, None) => return None,
                }
            }
            other => push_pieces(&mut out, vec![other.clone()]),
        }
    }
    Some(out)
}

/// One piece of a name: text the source writes, a parameter of the function
/// the name is written in, by position, or what a module-scope binding holds
/// (carrick#1562).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum TextPiece {
    Lit(String),
    Param(usize),
    /// A constant, an entry of a constant object (`TOPICS.orders`), or what
    /// a builder returns for the arguments it is handed
    /// (`topicFor("created")`, `ENDPOINTS.users.byId(id)`), read once every
    /// module's uses of the binding are known ([`LinkedNames`]).
    Named(Box<NamedRef>),
}

/// A module-scope binding a name is read through (carrick#1562).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct NamedRef {
    /// The file's own name for the binding: a module-scope `const` or
    /// function of the file, or an import binding.
    binding: String,
    /// The entries walked into it (`["users", "byId"]`).
    path: Vec<String>,
    /// The builder's arguments, each as text pieces, when it is called.
    args: Option<Vec<Vec<TextPiece>>>,
}

/// What a module-scope binding holds, as a name reads it (carrick#1562): a
/// constant's text, a constant object's entries, or a builder's returned
/// text with its parameters in it. Kept per module by name
/// ([`super::FileIr::names`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NameValue {
    Text(Vec<TextPiece>),
    Map(BTreeMap<String, NameValue>),
    Builder(Vec<TextPiece>),
}

impl NameValue {
    /// The entry `path` walks to: the value itself for an empty path.
    pub(super) fn entry(&self, path: &[String]) -> Option<&NameValue> {
        let mut at = self;
        for key in path {
            match at {
                NameValue::Map(entries) => at = entries.get(key)?,
                _ => return None,
            }
        }
        Some(at)
    }
}

/// A call written in the service, where a reading came through it
/// (carrick#1562): the own factory call that returned an instance
/// ([`SiteMaker::factory`]), or the library call whose name a caller fills
/// ([`LibrarySite::origin`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SiteCall {
    pub file: PathBuf,
    /// The call's span start, in SWC numbering.
    pub span_start: u32,
    pub line: u32,
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
    /// No binding: the call is made on the instance an own factory call
    /// before it returns (`createQueue("emails").add(…)`, carrick#1562). One
    /// made on what a package's maker returns is no site.
    Chained,
}

/// The maker call that built an instance, read where it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteMaker {
    pub form: MakerForm,
    /// The export's member the maker is (`None`: the export itself).
    pub member: Option<String>,
    /// What the maker was handed, read in the scope of the module that
    /// writes the maker call. Through an own factory, each parameter the
    /// factory handed the maker holds what the factory's caller passed.
    pub args: Vec<SiteArg>,
    /// The module that writes the maker call: the declaring module, or the
    /// own factory's.
    pub file: PathBuf,
    /// The maker call's span start (SWC numbering) and line: the definition a
    /// name the maker binds resolves to.
    pub span_start: u32,
    pub line: u32,
    pub holder: Holder,
    /// The own factory call the holder was set by, when the instance came
    /// out of one of the service's functions (carrick#1562): the call
    /// written where the instance is held, whatever factories it went
    /// through.
    pub factory: Option<SiteCall>,
    /// The module `let` a getter built the instance into on first use
    /// (carrick#1790): every call of the getter holds this one object, so
    /// every holder's uses are each site's ([`share_lazy_clients`]).
    pub shared: Option<SharedLet>,
}

/// A module-scope `let` that holds the one instance a getter builds on first
/// use and returns to every caller (carrick#1790).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SharedLet {
    /// The module that declares it.
    pub file: PathBuf,
    pub binding: String,
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

    /// The literal the maker was handed at argument `arg`, or at its key
    /// `key`: where a maker's `name`, `base` or `prefix` Slot reads
    /// (contract amendment 2, B2), so `new Queue("emails")` names `emails`.
    pub fn literal(&self, arg: usize, key: Option<&str>) -> Option<&str> {
        arg_literal(&self.args, arg, key)
    }

    /// Whether the maker was handed argument `arg`, or its key `key`.
    pub fn supplies(&self, arg: usize, key: Option<&str>) -> bool {
        arg_supplied(&self.args, arg, key)
    }
}

/// The receiver a site's call is made through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteReceiver {
    /// The package export itself.
    Export,
    /// An instance one of the export's makers built: one maker, or each of
    /// up to [`MAX_MAKERS`] when every path to the instance yields one of
    /// theirs and nothing else (carrick#1689, contract amendment 3 on
    /// carrick#1564), in the order the source writes them. Never empty. Two
    /// makers may share a receiver id and differ in what they were handed. A
    /// one-maker site is a set of one, read as before.
    Instance(Vec<SiteMaker>),
}

/// Which receivers a claim acts on (the contract's `on`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum On {
    Export,
    Instance,
    Both,
}

/// The receiver, member path and member an op claim names (the contract's
/// `on`, `of`, `path` and `member`). `on` and `of` are exclusive (contract
/// amendment 2, B3): an element carrying both, or neither, selects nothing.
#[derive(Debug, Clone, Copy)]
pub struct Selector<'a> {
    /// The export, every instance of every maker, or both.
    pub on: Option<On>,
    /// The one receiver the op acts on, by its receiver id
    /// ([`SiteMaker::receiver_id`], or `export`).
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
    /// Set on a site a caller fills (carrick#1562): a call to one of the
    /// service's functions whose library call takes a name from a
    /// parameter (`publish(topic, data)` calling `bus.publish(topic, data)`)
    /// is that library call again, at the caller, with the caller's
    /// arguments in the holes. `origin` is the library call. Everything but
    /// the arguments is the library call's; an argument's text is only what
    /// this call filled in, so what the library call states itself is never
    /// stated again here.
    pub origin: Option<SiteCall>,
}

impl LibrarySite {
    /// The contract's receiver ids the call may be made through, one check
    /// each (carrick#1689): `export`, or each of the set's makers'
    /// ([`SiteMaker::receiver_id`]), once, in the set's order.
    pub fn receiver_ids(&self) -> Vec<String> {
        match &self.receiver {
            SiteReceiver::Export => vec!["export".to_string()],
            SiteReceiver::Instance(makers) => {
                let mut ids: Vec<String> = Vec::new();
                for id in makers.iter().map(SiteMaker::receiver_id) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                ids
            }
        }
    }

    /// The makers whose instance the call may be made through: none for the
    /// export, one or more for an instance (carrick#1689).
    pub fn makers(&self) -> &[SiteMaker] {
        match &self.receiver {
            SiteReceiver::Export => &[],
            SiteReceiver::Instance(makers) => makers,
        }
    }

    /// What a message role reads off this site (carrick#1689, contract
    /// amendment 3): `read` is asked once for each receiver the call may be
    /// made through (`None`: the export; otherwise each maker of the set)
    /// and answers what a fact through that receiver states, or `None` when
    /// its maker, or every op element that applies to it, is unverified, or
    /// the elements it verifies read different names. A fact only when every
    /// receiver answers and all answer the same (op, name text, role, name
    /// scope); otherwise `None`, and the site stays a candidate.
    pub fn fold<R: PartialEq>(
        &self,
        mut read: impl FnMut(Option<&SiteMaker>) -> Option<R>,
    ) -> Option<R> {
        let readings: Vec<Option<R>> = match &self.receiver {
            SiteReceiver::Export => vec![read(None)],
            SiteReceiver::Instance(makers) => {
                makers.iter().map(|maker| read(Some(maker))).collect()
            }
        };
        let mut readings = readings.into_iter();
        let first = readings.next().flatten()?;
        for reading in readings {
            if reading? != first {
                return None;
            }
        }
        Some(first)
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

    /// Whether an op claim with `selector` acts on this site's call made
    /// through one of its receivers: the export (`through` is `None`), or the
    /// instance of `through`, one of the site's makers (carrick#1689). `of`
    /// names exactly one receiver by its id, and `on` applies otherwise
    /// (contract amendment 2, B3). An op is a call, never a construction.
    pub fn selected_by(&self, through: Option<&SiteMaker>, selector: &Selector<'_>) -> bool {
        let id = match (&self.receiver, through) {
            (SiteReceiver::Export, None) => "export".to_string(),
            (SiteReceiver::Instance(makers), Some(maker)) if makers.contains(maker) => {
                maker.receiver_id()
            }
            _ => return false,
        };
        let receiver = match (selector.on, selector.of) {
            (None, Some(of)) => of == id,
            (Some(on), None) => matches!(
                (through, on),
                (None, On::Export | On::Both) | (Some(_), On::Instance | On::Both)
            ),
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
    /// `classify` says what one member use is on a receiver (`receiver`: each
    /// of the site's ids, or `export` for the export an instance was made
    /// from). A use that can change a name, a prefix or a base contests, and
    /// so does one the package's surface does not list: nothing says it
    /// cannot. A use on an instance of a set is classified on every maker's
    /// receiver (carrick#1689).
    pub fn contest(&self, classify: impl Fn(&str, &MemberUse) -> MemberWire) -> Option<Contest> {
        if self.contested {
            return Some(Contest::Used);
        }
        let receivers = self.receiver_ids();
        let uses = self
            .uses
            .iter()
            .flat_map(|used| receivers.iter().map(move |on| (on.as_str(), used)))
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

/// How many own factory calls an instance is followed through, and how many
/// callers up a name's hole is filled through (carrick#1562): the call
/// graph's re-export cap, for the same reason.
const MAX_VALUE_HOPS: usize = super::MAX_REEXPORT_HOPS;

/// How many makers' instances a receiver may be one of (carrick#1689,
/// contract amendment 3 on carrick#1564): more reads as nothing.
pub(super) const MAX_MAKERS: usize = 4;

/// Every call and construction the service makes through a package export or
/// an instance of one ([`LibrarySite`]), with every call to one of the
/// service's functions that fills a name that function's library call takes
/// from a parameter ([`LibrarySite::origin`]).
pub fn library_sites(inputs: &RequestSummaryInputs) -> LibrarySites {
    let clients = LinkedClients::link(&inputs.files, &inputs.bindings);
    let names = LinkedNames::link(&inputs.files, &inputs.bindings);
    let callers = Callers::index(inputs);
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
            names: &names,
            inputs,
            callers: &callers,
        };
        for key in keys {
            reader.collect(&file.functions[key], Some(key), &mut sites);
        }
        reader.collect(&file.module_level, None, &mut sites);
    }
    share_lazy_clients(&mut sites, &callers, inputs);
    sites.sort_by(|a, b| {
        (&a.file, a.span_start, a.form, a.span_end, &a.origin).cmp(&(
            &b.file,
            b.span_start,
            b.form,
            b.span_end,
            &b.origin,
        ))
    });
    LibrarySites { sites }
}

/// Every holder of a getter's one instance holds one object (carrick#1790),
/// so a use any holder makes is a use at every site through it, as every
/// module's uses of a module-scope instance are ([`LinkedClients`]): each
/// site through a [`SharedLet`] takes every such site's member uses and
/// contest. A call of the getter that no site reads the instance through
/// holds it where nothing here sees its uses (`register(getClient())`, or a
/// factory that returns the call), so it takes the instance away from
/// every site. A call the call graph does not resolve is not seen at all.
fn share_lazy_clients(
    sites: &mut [LibrarySite],
    callers: &Callers<'_>,
    inputs: &RequestSummaryInputs,
) {
    #[derive(Default)]
    struct Shared {
        contested: bool,
        uses: BTreeSet<MemberUse>,
        export_uses: BTreeSet<MemberUse>,
        /// The getter calls a site reads the instance through.
        read_at: HashSet<(PathBuf, u32)>,
    }
    let mut shared: BTreeMap<SharedLet, Shared> = BTreeMap::new();
    for site in sites.iter() {
        for maker in site.makers() {
            let Some(held) = &maker.shared else {
                continue;
            };
            let group = shared.entry(held.clone()).or_default();
            group.contested |= site.contested;
            group.uses.extend(site.uses.iter().cloned());
            group.export_uses.extend(site.export_uses.iter().cloned());
            if let Some(call) = &maker.factory {
                group.read_at.insert((call.file.clone(), call.span_start));
            }
        }
    }
    for (held, group) in &mut shared {
        let functions = inputs.files.get(&held.file).map(|ir| &ir.functions);
        for (key, function) in functions.into_iter().flatten() {
            let getter = function.returned.as_ref().is_some_and(|returned| {
                returned
                    .makers
                    .iter()
                    .any(|maker| maker.shared.as_deref() == Some(held.binding.as_str()))
            });
            if !getter {
                continue;
            }
            for caller in callers.of(&held.file, key) {
                let at = (caller.file.to_path_buf(), caller.call.site.span_start);
                if !group.read_at.contains(&at) {
                    group.contested = true;
                }
            }
        }
    }
    for site in sites.iter_mut() {
        let held: Vec<SharedLet> = site
            .makers()
            .iter()
            .filter_map(|maker| maker.shared.clone())
            .collect();
        for held in held {
            let group = &shared[&held];
            site.contested |= group.contested;
            site.uses.extend(group.uses.iter().cloned());
            site.export_uses.extend(group.export_uses.iter().cloned());
        }
    }
}

struct SiteReader<'a> {
    file: &'a Path,
    ir: &'a FileIr,
    clients: &'a LinkedClients,
    names: &'a LinkedNames<'a>,
    inputs: &'a RequestSummaryInputs,
    callers: &'a Callers<'a>,
}

/// The instance a holder holds, read where its package maker is written:
/// through every own factory it came out of (carrick#1562).
struct Made {
    package: String,
    export: String,
    /// The package maker's call, with each factory parameter it was handed
    /// filled with what the factory's caller passed.
    instance: ClientInstance,
    /// The module that writes the maker call.
    file: PathBuf,
    /// The own call the holder was set by.
    factory: Option<SiteCall>,
    /// A factory's binding of the instance is used in a way that may change
    /// it, or returned anywhere but by the factory itself.
    contested: bool,
    /// The members each factory calls through its binding of the instance.
    uses: BTreeSet<MemberUse>,
    /// The export's binding's member uses where the maker is written.
    export_uses: BTreeSet<MemberUse>,
    /// The module `let` a getter built the instance into (carrick#1790).
    shared: Option<SharedLet>,
}

impl Made {
    /// Whether two are one export's maker, handed the same arguments: one
    /// maker of a set (carrick#1689), wherever each was written. An instance
    /// a getter shares and a fresh one are two (carrick#1790).
    fn same_maker(&self, other: &Made) -> bool {
        self.package == other.package
            && self.export == other.export
            && self.instance.form == other.instance.form
            && self.instance.member == other.instance.member
            && self.instance.args == other.instance.args
            && self.shared == other.shared
    }

    /// `other`'s contest and uses added to these: the first one written
    /// stands for both.
    fn absorb(&mut self, other: Made) {
        self.contested |= other.contested;
        self.uses.extend(other.uses);
        self.export_uses.extend(other.export_uses);
    }
}

impl SiteReader<'_> {
    /// Every library site `ir` writes, and those its callers fill. `key` is
    /// the function's definition key when `ir` is a keyed function's own
    /// body: only its parameters are what a call of that key passes.
    fn collect(&self, ir: &FnIr, key: Option<&str>, out: &mut Vec<LibrarySite>) {
        for site in &ir.library {
            // A call the call graph resolves to a function of this service is
            // that function's, whatever its receiver's name.
            if site.form == MakerForm::Call && self.resolves(self.file, &site.site).is_some() {
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
                SiteBinding::Chained(client) => (&**client, self.file, Holder::Chained),
            };
            // A return hands the receiver to whoever called the function
            // that returned it.
            let contested = client.contested_message || !client.returned.is_empty();
            let args = self.names.resolved_all(self.file, &site.args);
            let library = match &client.instance {
                None => LibrarySite {
                    file: self.file.to_path_buf(),
                    span_start: site.site.span_start,
                    span_end: site.site.span_end,
                    line: site.op_line,
                    form: site.form,
                    specifier: client.package.clone(),
                    export: client.export.clone(),
                    receiver: SiteReceiver::Export,
                    path: site.path.clone(),
                    member: site.member.clone(),
                    args,
                    contested,
                    uses: client.member_uses.clone(),
                    export_uses: client.export_uses.clone(),
                    origin: None,
                },
                Some(_) => {
                    // What an own call holds is what the function returns,
                    // and a call to one that is no own factory holds nothing
                    // a package made.
                    let made = self.made(declared_in, client, 0);
                    let Some(first) = made.first() else {
                        continue;
                    };
                    // The makers of a set are one export's (carrick#1689): a
                    // set across exports or packages is read as nothing.
                    let (specifier, export) = (first.package.clone(), first.export.clone());
                    if made
                        .iter()
                        .any(|made| made.package != specifier || made.export != export)
                    {
                        continue;
                    }
                    // A call made on what a package's maker returns
                    // (`z.string().min(1)`) is no receiver form the contract
                    // names; one made on what an own factory returns is
                    // (carrick#1562).
                    if holder == Holder::Chained && first.factory.is_none() {
                        continue;
                    }
                    let mut uses = client.member_uses.clone();
                    let mut export_uses = BTreeSet::new();
                    let mut made_contested = false;
                    let makers = made
                        .into_iter()
                        .map(|made| {
                            uses.extend(made.uses);
                            export_uses.extend(made.export_uses);
                            made_contested |= made.contested;
                            SiteMaker {
                                form: made.instance.form,
                                member: made.instance.member,
                                args: made.instance.args,
                                file: made.file,
                                span_start: made.instance.site.span_start,
                                line: made.instance.site.line,
                                holder,
                                factory: made.factory,
                                shared: made.shared,
                            }
                        })
                        .collect();
                    LibrarySite {
                        file: self.file.to_path_buf(),
                        span_start: site.site.span_start,
                        span_end: site.site.span_end,
                        line: site.op_line,
                        form: site.form,
                        specifier,
                        export,
                        receiver: SiteReceiver::Instance(makers),
                        path: site.path.clone(),
                        member: site.member.clone(),
                        args,
                        contested: contested || made_contested,
                        uses,
                        export_uses,
                        origin: None,
                    }
                }
            };
            // A name taken from a parameter is filled where the function is
            // called (carrick#1562).
            if let Some(key) = key
                && library.args.iter().any(SiteArg::has_holes)
            {
                let mut chain = Vec::new();
                self.fill_from_callers(&library, &library.args, (self.file, key), &mut chain, out);
            }
            out.push(library);
        }
        for nested in ir.nested.iter().chain(&ir.detached) {
            self.collect(nested, None, out);
        }
    }

    /// The function of the service the call at `site` in `file` reaches,
    /// when the call graph resolved it: this call's, never the one before it
    /// in a chain, which starts where it does.
    fn resolves(&self, file: &Path, site: &Site) -> Option<&(PathBuf, String)> {
        self.inputs.sites.target_at(file, site.lo, site.hi)
    }

    /// The instance `client` (held in `file`) holds, where a package's maker
    /// made it (carrick#1562): one per maker it may come from (carrick#1689),
    /// empty when it holds nothing a package made. A maker call the call
    /// graph resolves to a function of the service is an own call: what it
    /// holds is what that function returns ([`super::FnIr::returned`]), with
    /// the function's parameters filled by this call's arguments, through as
    /// many own factories as it took. An own call to anything else, an async
    /// factory called without `await`, one past the hop cap, and one whose
    /// makers come to more than [`MAX_MAKERS`] hold nothing. One maker,
    /// handed the same arguments, reached twice is one maker, used as both
    /// are.
    fn made(&self, file: &Path, client: &ClientRef, depth: usize) -> Vec<Made> {
        let Some(instance) = client.instance.as_ref() else {
            return Vec::new();
        };
        let target = match instance.form {
            MakerForm::Call => self.resolves(file, &instance.site),
            MakerForm::New => None,
        };
        let Some((target_file, key)) = target else {
            return match instance.made_by {
                MadeBy::Export => vec![Made {
                    package: client.package.clone(),
                    export: client.export.clone(),
                    instance: ClientInstance {
                        args: self.names.resolved_all(file, &instance.args),
                        ..instance.clone()
                    },
                    file: file.to_path_buf(),
                    factory: None,
                    contested: false,
                    uses: BTreeSet::new(),
                    export_uses: client.export_uses.clone(),
                    shared: None,
                }],
                MadeBy::Call(_) => Vec::new(),
            };
        };
        if depth >= MAX_VALUE_HOPS {
            return Vec::new();
        }
        let Some(returned) = self
            .inputs
            .files
            .get(target_file)
            .and_then(|ir| ir.functions.get(key))
            .and_then(|function| function.returned.as_ref())
        else {
            return Vec::new();
        };
        if returned.is_async && !instance.awaited {
            return Vec::new();
        }
        let passed = self.names.resolved_all(file, &instance.args);
        let factory = SiteCall {
            file: file.to_path_buf(),
            span_start: instance.site.span_start,
            line: instance.site.line,
        };
        let mut out: Vec<Made> = Vec::new();
        for product in &returned.makers {
            let mut inner = self.made(target_file, product, depth + 1);
            if inner.is_empty() {
                return Vec::new();
            }
            // A getter's one instance (carrick#1790): every call holds it.
            // One another getter built is held through two `let`s, which
            // nothing here joins.
            if let Some(binding) = &product.shared {
                for made in &mut inner {
                    if made.shared.is_some() {
                        return Vec::new();
                    }
                    made.shared = Some(SharedLet {
                        file: target_file.clone(),
                        binding: binding.clone(),
                    });
                }
            }
            // The factory's binding is used to change the instance, or
            // returned somewhere the factory's own returns are not: a caller
            // of that other function may hold it too.
            let contested = product.contested_message || !product.returned.is_subset(&returned.at);
            for mut made in inner {
                made.instance.args = made
                    .instance
                    .args
                    .iter()
                    .map(|arg| arg.filled(&passed, true).0)
                    .collect();
                made.contested |= contested;
                made.uses.extend(product.member_uses.iter().cloned());
                made.factory = Some(factory.clone());
                match out.iter_mut().find(|known| known.same_maker(&made)) {
                    Some(known) => known.absorb(made),
                    None => out.push(made),
                }
            }
        }
        if out.len() > MAX_MAKERS {
            return Vec::new();
        }
        out
    }

    /// Every call of `function` that fills a hole in `args` (the arguments of
    /// `origin`'s library call, as far as the callers between have filled
    /// them), stated as `origin` again at the call (carrick#1562). A call
    /// that fills a hole with text states it; one that passes its own
    /// parameter on leaves the hole open for its own callers, up to the hop
    /// cap. A spread argument at a call fills nothing.
    fn fill_from_callers(
        &self,
        origin: &LibrarySite,
        args: &[SiteArg],
        function: (&Path, &str),
        chain: &mut Vec<(PathBuf, String)>,
        out: &mut Vec<LibrarySite>,
    ) {
        if chain.len() >= MAX_VALUE_HOPS {
            return;
        }
        chain.push((function.0.to_path_buf(), function.1.to_string()));
        for caller in self.callers.of(function.0, function.1) {
            let Some(passed) = &caller.call.site_args else {
                continue;
            };
            let passed = self.names.resolved_all(caller.file, passed);
            let mut filled = false;
            let derived: Vec<SiteArg> = args
                .iter()
                .map(|arg| {
                    let (arg, any) = arg.filled(&passed, false);
                    filled |= any;
                    arg
                })
                .collect();
            if filled {
                out.push(LibrarySite {
                    file: caller.file.to_path_buf(),
                    span_start: caller.call.site.span_start,
                    span_end: caller.call.site.span_end,
                    line: caller.call.name_line,
                    args: derived.clone(),
                    origin: Some(SiteCall {
                        file: origin.file.clone(),
                        span_start: origin.span_start,
                        line: origin.line,
                    }),
                    ..origin.clone()
                });
            }
            if let Some(key) = caller.key
                && derived.iter().any(SiteArg::has_holes)
                && !chain
                    .iter()
                    .any(|(file, at)| file.as_path() == caller.file && at == key)
            {
                self.fill_from_callers(origin, &derived, (caller.file, key), chain, out);
            }
        }
        chain.pop();
    }
}

/// What every module-scope binding a name reads through holds, with every
/// module's uses of it known (carrick#1562): a constant's text, a constant
/// object's entry, or what a builder returns.
///
/// A constant's text cannot change. A constant object can, through any
/// module that reaches it, so one is read only where every module that
/// imports it keeps it ([`super::BindingUse::keeps_entries`]), no module
/// reaches its module through a namespace import or loads it some other way,
/// and, read in another module, where the scan can follow every module of
/// the service (as for an imported instance, [`LinkedClients`]).
pub(super) struct LinkedNames<'a> {
    files: &'a HashMap<PathBuf, FileIr>,
    bindings: &'a super::ImportedBindings,
    /// Constant objects some module's use may change, by declaring module
    /// and name.
    contested: HashSet<(PathBuf, String)>,
    /// Some module names a specifier the scan cannot follow.
    unfollowable: bool,
}

impl<'a> LinkedNames<'a> {
    pub(super) fn link(
        files: &'a HashMap<PathBuf, FileIr>,
        bindings: &'a super::ImportedBindings,
    ) -> Self {
        let mut linked = LinkedNames {
            files,
            bindings,
            contested: HashSet::new(),
            unfollowable: !bindings.unresolved_in().is_empty(),
        };
        let mut every: Vec<(PathBuf, String)> = Vec::new();
        for (file, ir) in files {
            for (local, used) in &ir.imported {
                match bindings.get(file, local) {
                    Some(super::ImportedBinding::Binding { file: at, name }) => {
                        match linked.declared(at, name, 0) {
                            // Only an object can be changed: text and a
                            // builder's return cannot.
                            Some((declared, value)) => {
                                if matches!(value, NameValue::Map(_)) && !used.keeps_entries(value)
                                {
                                    linked.contested.insert(declared);
                                }
                            }
                            None => linked.unfollowable |= linked.unfollowed(at, name, 0),
                        }
                    }
                    Some(super::ImportedBinding::Module(published)) => {
                        every.extend(
                            published
                                .iter()
                                .map(|binding| (binding.file.clone(), binding.name.clone())),
                        );
                    }
                    Some(
                        super::ImportedBinding::Unfollowable | super::ImportedBinding::Unresolved,
                    ) => linked.unfollowable = true,
                    None => {}
                }
            }
            for specifier in &ir.loads {
                if let Some(super::ImportedBinding::Module(published)) =
                    bindings.load(file, specifier)
                {
                    every.extend(
                        published
                            .iter()
                            .map(|binding| (binding.file.clone(), binding.name.clone())),
                    );
                }
            }
        }
        // A namespace import, or a module loaded some other way, may reach
        // any object its module publishes in any way.
        for (file, name) in every {
            if let Some((declared, NameValue::Map(_))) = linked.declared(&file, &name, 0) {
                linked.contested.insert(declared);
            }
        }
        linked
    }

    /// What `name` in `file` holds, following a module that re-exports an
    /// import, with the module and name that declare it.
    fn declared(
        &self,
        file: &Path,
        name: &str,
        depth: usize,
    ) -> Option<((PathBuf, String), &'a NameValue)> {
        let ir = self.files.get(file)?;
        if let Some(value) = ir.names.get(name) {
            return Some(((file.to_path_buf(), name.to_string()), value));
        }
        if depth >= super::MAX_REEXPORT_HOPS || !ir.imported.contains_key(name) {
            return None;
        }
        match self.bindings.get(file, name)? {
            super::ImportedBinding::Binding { file, name } => self.declared(file, name, depth + 1),
            _ => None,
        }
    }

    /// Whether following `name` in `file` stopped where the scan cannot see
    /// what is there, rather than at something that is no name.
    fn unfollowed(&self, file: &Path, name: &str, depth: usize) -> bool {
        let Some(ir) = self.files.get(file) else {
            return false;
        };
        if ir.names.contains_key(name) || !ir.imported.contains_key(name) {
            return false;
        }
        if depth >= super::MAX_REEXPORT_HOPS {
            return true;
        }
        match self.bindings.get(file, name) {
            Some(super::ImportedBinding::Binding { file, name }) => {
                self.unfollowed(file, name, depth + 1)
            }
            Some(super::ImportedBinding::Unfollowable | super::ImportedBinding::Unresolved) => true,
            _ => false,
        }
    }

    /// `pieces`, read in `file`, with every named piece read through the
    /// binding it names: text, or a parameter of the function `pieces` is
    /// written in. `None` when one names nothing this can read.
    fn resolve(&self, file: &Path, pieces: &[TextPiece], depth: usize) -> Option<Vec<TextPiece>> {
        let mut out = Vec::new();
        for piece in pieces {
            let TextPiece::Named(named) = piece else {
                push_pieces(&mut out, vec![piece.clone()]);
                continue;
            };
            if depth >= MAX_VALUE_HOPS {
                return None;
            }
            let ir = self.files.get(file)?;
            let own = ir.names.contains_key(&named.binding);
            if !own && self.unfollowable {
                return None;
            }
            let ((declared_in, declared), value) = self.declared(file, &named.binding, 0)?;
            if self
                .contested
                .contains(&(declared_in.clone(), declared.clone()))
            {
                return None;
            }
            let value = value.entry(&named.path)?;
            let read = match (value, &named.args) {
                (NameValue::Text(text), None) => self.resolve(&declared_in, text, depth + 1)?,
                (NameValue::Builder(body), Some(args)) => {
                    // The arguments are read where the builder is called,
                    // then handed to its parameters.
                    let args: Vec<SiteArg> = args
                        .iter()
                        .map(|arg| {
                            let (text, holes) = split_text(self.resolve(file, arg, depth + 1));
                            SiteArg {
                                text,
                                holes,
                                ..SiteArg::default()
                            }
                        })
                        .collect();
                    let body = self.resolve(&declared_in, body, depth + 1)?;
                    fill_pieces(&body, &args)?
                }
                _ => return None,
            };
            push_pieces(&mut out, read);
        }
        Some(out)
    }

    /// `arg`, read in `file`, with every named piece read
    /// ([`Self::resolve`]).
    fn resolved(&self, file: &Path, arg: &SiteArg) -> SiteArg {
        let read = |pieces: &Vec<TextPiece>| split_text(self.resolve(file, pieces, 0));
        let (text, holes) = match &arg.holes {
            Some(pieces) => read(pieces),
            None => (arg.text.clone(), None),
        };
        let object = arg.object.as_ref().map(|object| {
            let mut out = object.clone();
            for (key, pieces) in &object.holes {
                let (text, holes) = read(pieces);
                out.fields.insert(key.clone(), text);
                match holes {
                    Some(holes) => out.holes.insert(key.clone(), holes),
                    None => out.holes.remove(key),
                };
            }
            out
        });
        SiteArg {
            text,
            object,
            function: arg.function,
            holes,
        }
    }

    fn resolved_all(&self, file: &Path, args: &[SiteArg]) -> Vec<SiteArg> {
        args.iter().map(|arg| self.resolved(file, arg)).collect()
    }
}

/// Every call the call graph resolves to a function of the service, by the
/// function it reaches (carrick#1562).
struct Callers<'a> {
    by_target: HashMap<(PathBuf, String), Vec<Caller<'a>>>,
}

/// One call of a function of the service.
struct Caller<'a> {
    /// The file the call is written in.
    file: &'a Path,
    call: &'a super::CallIr,
    /// The definition key of the function whose own body writes the call:
    /// its parameters are what its own callers pass. `None` for a call at
    /// module level or in a function written inside another.
    key: Option<&'a str>,
}

impl<'a> Callers<'a> {
    fn index(inputs: &'a RequestSummaryInputs) -> Self {
        fn walk<'a>(
            inputs: &'a RequestSummaryInputs,
            file: &'a Path,
            ir: &'a FnIr,
            key: Option<&'a str>,
            by_target: &mut HashMap<(PathBuf, String), Vec<Caller<'a>>>,
        ) {
            for call in &ir.calls {
                if let Some(target) = inputs.sites.target_at(file, call.site.lo, call.site.hi) {
                    by_target
                        .entry(target.clone())
                        .or_default()
                        .push(Caller { file, call, key });
                }
            }
            for nested in ir.nested.iter().chain(&ir.detached) {
                walk(inputs, file, nested, None, by_target);
            }
        }
        let mut by_target = HashMap::new();
        let mut paths: Vec<&PathBuf> = inputs.files.keys().collect();
        paths.sort();
        for path in paths {
            let file = &inputs.files[path];
            let mut keys: Vec<&String> = file.functions.keys().collect();
            keys.sort();
            for key in keys {
                walk(
                    inputs,
                    path,
                    &file.functions[key],
                    Some(key.as_str()),
                    &mut by_target,
                );
            }
            walk(inputs, path, &file.module_level, None, &mut by_target);
        }
        Callers { by_target }
    }

    /// The calls of the function `key` in `file`.
    fn of(&self, file: &Path, key: &str) -> &[Caller<'a>] {
        self.by_target
            .get(&(file.to_path_buf(), key.to_string()))
            .map_or(&[], Vec::as_slice)
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
    /// The instance the expression before the member returns
    /// (`createQueue("emails").add(…)`, carrick#1562), read as a holder's
    /// value is ([`Reader::written_instance`]). Nothing else uses it.
    Chained(Box<ClientRef>),
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
    /// `client.member(…)`, `client.a.b.member(…)`, the same on a class
    /// field ([`field_binding`]), and the same on what a maker call or an
    /// own call returns (`createQueue("emails").add(…)`, carrick#1562).
    /// Every hop is a plain name; anything computed is no site.
    ///
    /// The one place a library receiver is identified.
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
                Expr::Call(_) | Expr::New(_) | Expr::Await(_) => {
                    path.reverse();
                    let client = self.written_instance(obj, scope)?;
                    return Some((
                        SiteBinding::Chained(Box::new(client)),
                        path,
                        Some(member),
                        named,
                    ));
                }
                _ => return None,
            }
        }
    }

    pub(super) fn line(&self, span: Span) -> u32 {
        self.source_map.lookup_char_pos(span.lo).line as u32
    }

    /// The instance a binding or a class field is set to, when the source
    /// states one: a maker call on a package export
    /// ([`Reader::factory_call`]), or a call to a function the service may
    /// declare, whose return is read once the call graph says which function
    /// it is (carrick#1562, [`Reader::own_call`]).
    ///
    /// The one place what a holder is set to is read: a module or a
    /// function's `const`, a class field, and a call made on what a call
    /// returns.
    pub(super) fn written_instance(&self, expr: &Expr, scope: &Scope<'_>) -> Option<ClientRef> {
        self.factory_call(expr, scope)
            .or_else(|| self.own_call(expr, scope))
    }

    /// What a `return` hands its caller, when it is an instance each call
    /// builds anew (carrick#1562): one [`Reader::written_instance`] reads, a
    /// local that holds one, or a `let` set on every path to one of a few
    /// makers' (carrick#1689, [`Reader::let_makers`]), one per maker, or the
    /// module `let` a getter builds a client into on first use
    /// (carrick#1790, [`Reader::lazy_clients`]), whose one instance every
    /// call shares. Empty for anything else: a parameter, a module's `const`
    /// instance and a class field are shared by every call, and nothing
    /// reads their other holders' uses. Only a function the call graph keys is
    /// followed as a factory, and such a function is never written inside
    /// another, so every local it holds is its own.
    pub(super) fn returned_instances(&self, expr: &Expr, scope: &Scope<'_>) -> Vec<ClientRef> {
        if let Expr::Ident(ident) = unwrap_expression(expr) {
            let key = ident_key(ident);
            if let Some(makers) = scope.let_makers.get(&key) {
                return makers.clone();
            }
            if let Some(client) = scope.local_receivers.get(&key) {
                return vec![client.clone()];
            }
            // The one instance a getter builds on first use (carrick#1790):
            // shared by every call, which [`share_lazy_clients`] reads.
            return scope
                .module
                .lazy_clients
                .get(&key)
                .cloned()
                .into_iter()
                .collect();
        }
        self.written_instance(expr, scope).into_iter().collect()
    }

    /// The instances a `let` (or `var`) holds wherever the function returns
    /// it, when the body sets it to an instance on every path (carrick#1689),
    /// one per write: `binding` is declared once, by the statement before
    /// `after`, with no initialiser or an instance; every other write is a
    /// plain `binding = <instance>` statement in the function's own
    /// statements, or in blocks and `if` branches among them; and every
    /// `return binding` in the body is one of those statements too, reached
    /// only once the binding is set. A write anywhere else (a loop, a `try`,
    /// a `switch`, a nested function, `||=`, `++`) leaves it read as nothing,
    /// and so does a return the walk does not reach, a nested function's
    /// included. Every maker is
    /// read in the scope the binding is declared in. How many makers that
    /// comes to is [`super::FnIr::settle_returned`]'s to judge.
    ///
    /// A path may set the binding twice; both makers are in the set, which a
    /// fold over the set reads as strictly as it reads each of them.
    pub(super) fn let_makers<'s>(
        &self,
        body: &'s [Stmt],
        after: &'s [Stmt],
        binding: &Ident,
        init: Option<&'s Expr>,
        scope: &Scope<'_>,
    ) -> Option<Vec<ClientRef>> {
        let key = ident_key(binding);
        let mut flow = LetFlow {
            key: &key,
            writes: init.into_iter().collect(),
            claimed: HashSet::new(),
            returns: 0,
        };
        flow.block(after, init.is_some())?;
        let mut uses = LetUses {
            key: &key,
            claimed: &flow.claimed,
            unclaimed: false,
            returns: 0,
        };
        for stmt in body {
            stmt.visit_with(&mut uses);
        }
        if uses.unclaimed || uses.returns != flow.returns {
            return None;
        }
        flow.writes
            .into_iter()
            .map(|write| self.written_instance(write, scope))
            .collect()
    }

    /// Each module-scope `let` (or `var`) a getter builds a client into on
    /// first use (carrick#1790), with the instance it holds:
    ///
    /// ```ts
    /// let client: Client | null = null;
    /// export function getClient() {
    ///   if (!client) { client = new Client(options); }
    ///   return client;
    /// }
    /// ```
    ///
    /// It is one when it is declared once, unexported, with no initialiser,
    /// `null` or `undefined`; every write to it is a statement `x = …`,
    /// `x ??= …` or `x ||= …` of one maker, handed the same arguments (read
    /// in the module's scope, as a module `const` is); and the file uses it
    /// only to test it, compare it and return it. So it holds nothing or that
    /// one instance, whatever runs first, and a call through it runs only on
    /// the instance. Anything else (another value written anywhere, a
    /// destructuring or `for (x of …)` write, `x++`, a call or a member read
    /// through it, a hand-off, an export, a second declaration of the name)
    /// leaves it no client, as before.
    ///
    /// No call is read through the `let` itself: it is read only where a
    /// function returns it ([`Reader::returned_instances`]), and every call
    /// of that function holds the one instance ([`share_lazy_clients`]).
    pub(super) fn lazy_clients(
        &self,
        module: &Module,
        module_scope: &ModuleScope,
        reassigned: &super::Reassigned,
    ) -> HashMap<BindingKey, ClientRef> {
        let declared = crate::binding_scope::Declarations::of(module);
        let scope = Scope::module(module_scope);
        let mut out = HashMap::new();
        for item in &module.body {
            // `export let` publishes the binding: an importer may write it.
            // A `const` is never written, so it has no write to read.
            let ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) = item else {
                continue;
            };
            for declarator in &var.decls {
                let Pat::Ident(ident) = &declarator.name else {
                    continue;
                };
                if !declarator.init.as_deref().is_none_or(holds_nothing) {
                    continue;
                }
                let name = ident.id.sym.as_ref();
                let key = ident_key(&ident.id);
                // Every other place the name is bound is a plain
                // assignment's target ([`crate::binding_scope::Declarations`]
                // counts those): no scope declares it again, and no pattern
                // or loop head writes it.
                let targets = reassigned.targets.get(name).copied().unwrap_or(0);
                if declared.name_count(name) != 1 + targets {
                    continue;
                }
                if !module_scope
                    .uses
                    .get(name)
                    .is_some_and(only_tested_and_returned)
                {
                    continue;
                }
                let mut writes = LazyWrites {
                    key: &key,
                    writes: Vec::new(),
                    updated: false,
                };
                module.visit_with(&mut writes);
                if writes.updated || writes.writes.is_empty() || writes.writes.len() != targets {
                    continue;
                }
                let Some(made) = writes
                    .writes
                    .iter()
                    .map(|write| self.written_instance(write, &scope))
                    .collect::<Option<Vec<ClientRef>>>()
                else {
                    continue;
                };
                let first = &made[0];
                if !made.iter().all(|client| same_maker(client, first)) {
                    continue;
                }
                let mut client = module_scope.with_uses(first.clone(), name);
                client.shared = Some(name.to_string());
                out.insert(key, client);
            }
        }
        out
    }

    /// A call to a function the service may declare, as a holder's value
    /// (carrick#1562): `make(…)`, `this.make(…)`, `this.#make(…)` or
    /// `ns.a.make(…)`, awaited or not. What it holds is the instance the
    /// function returns ([`super::FnIr::returned`]) when the call graph
    /// resolves the call to an own factory, and nothing otherwise. A spread
    /// argument moves every position, and a parameter or a computed member
    /// names no function, so neither is one.
    fn own_call(&self, expr: &Expr, scope: &Scope<'_>) -> Option<ClientRef> {
        let (awaited, expr) = match unwrap_expression(expr) {
            Expr::Await(awaited) => (true, unwrap_expression(&awaited.arg)),
            other => (false, other),
        };
        let Expr::Call(call) = expr else {
            return None;
        };
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        // `require("pkg")` loads a module: the binding is the import it is
        // read as ([`super::import_bindings`]), never a function's return.
        if call.args.iter().any(|arg| arg.spread.is_some())
            || matches!(unwrap_expression(callee), Expr::Ident(ident) if ident.sym == *"require")
        {
            return None;
        }
        let callee = own_callee(callee, scope)?;
        Some(ClientRef {
            package: String::new(),
            export: String::new(),
            instance: Some(ClientInstance {
                form: MakerForm::Call,
                member: None,
                options: None,
                args: call
                    .args
                    .iter()
                    .map(|arg| site_arg(&arg.expr, scope))
                    .collect(),
                site: self.site(call.span),
                made_by: MadeBy::Call(callee),
                awaited,
            }),
            contested: false,
            called: BTreeSet::new(),
            called_computed: false,
            contested_message: false,
            member_uses: BTreeSet::new(),
            export_uses: BTreeSet::new(),
            returned: BTreeSet::new(),
            shared: None,
        })
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

/// The function an own call's callee names, as the source writes it
/// ([`OwnCallee`]): a binding that is not a parameter, or a chain of plain
/// members on one or on `this`.
fn own_callee(callee: &Expr, scope: &Scope<'_>) -> Option<OwnCallee> {
    let mut path: Vec<String> = Vec::new();
    let mut at = unwrap_expression(callee);
    loop {
        match at {
            Expr::Ident(ident) if path.is_empty() => {
                if scope.param_index(ident).is_some() {
                    return None;
                }
                return Some(OwnCallee::Binding(ident_key(ident)));
            }
            Expr::Ident(ident) => {
                if scope.param_index(ident).is_some() {
                    return None;
                }
                path.reverse();
                return Some(OwnCallee::Member(Some(ident_key(ident)), path));
            }
            Expr::This(_) if !path.is_empty() => {
                path.reverse();
                return Some(OwnCallee::Member(None, path));
            }
            Expr::Member(member) => {
                path.push(this_field(member)?);
                at = unwrap_expression(&member.obj);
            }
            _ => return None,
        }
    }
}

/// The walk [`Reader::let_makers`] makes of a function's own statements
/// after a `let`'s declaration: what each plain write sets it to, and the
/// `return`s of it reached once it is set (carrick#1689).
struct LetFlow<'a> {
    key: &'a BindingKey,
    /// What the binding is set to: its initialiser, then each write the walk
    /// claims.
    writes: Vec<&'a Expr>,
    /// Where each claimed write starts.
    claimed: HashSet<u32>,
    /// The `return <binding>` statements reached with the binding set.
    returns: usize,
}

/// How a run of statements ends: the binding set or not, or every path out
/// of it a `return` or a `throw`.
enum Reach {
    Set(bool),
    Exits,
}

impl<'a> LetFlow<'a> {
    /// `stmts` in order, from `set`. `None` when a `return` of the binding
    /// may run before it is set. Statements after an exit are never reached.
    fn block(&mut self, stmts: &'a [Stmt], set: bool) -> Option<Reach> {
        let mut set = set;
        for stmt in stmts {
            match self.stmt(stmt, set)? {
                Reach::Exits => return Some(Reach::Exits),
                Reach::Set(now) => set = now,
            }
        }
        Some(Reach::Set(set))
    }

    fn stmt(&mut self, stmt: &'a Stmt, set: bool) -> Option<Reach> {
        match stmt {
            Stmt::Expr(ExprStmt { expr, .. }) => {
                if let Expr::Assign(assign) = unwrap_expression(expr)
                    && assign.op == AssignOp::Assign
                    && let AssignTarget::Simple(SimpleAssignTarget::Ident(target)) = &assign.left
                    && ident_key(&target.id) == *self.key
                {
                    self.writes.push(&assign.right);
                    self.claimed.insert(assign.span.lo.0);
                    return Some(Reach::Set(true));
                }
                Some(Reach::Set(set))
            }
            Stmt::Return(ret) => {
                if ret
                    .arg
                    .as_deref()
                    .is_some_and(|arg| names_binding(arg, self.key))
                {
                    if !set {
                        return None;
                    }
                    self.returns += 1;
                }
                Some(Reach::Exits)
            }
            Stmt::Throw(_) => Some(Reach::Exits),
            Stmt::Block(block) => self.block(&block.stmts, set),
            Stmt::If(branch) => {
                let cons = self.stmt(&branch.cons, set)?;
                let alt = match &branch.alt {
                    Some(alt) => self.stmt(alt, set)?,
                    None => Reach::Set(set),
                };
                Some(match (cons, alt) {
                    (Reach::Exits, other) | (other, Reach::Exits) => other,
                    (Reach::Set(a), Reach::Set(b)) => Reach::Set(a && b),
                })
            }
            // Anything else leaves the binding as it was: a write or a
            // `return` of it inside is one the walk does not claim, which
            // [`LetUses`] finds.
            _ => Some(Reach::Set(set)),
        }
    }
}

/// Every write of a `let` anywhere in a function body, and every `return
/// <binding>`, nested functions included (carrick#1689): a write
/// [`LetFlow`] did not claim, or a return it did not reach, leaves the
/// binding read as nothing. A destructuring write or a
/// `for (x of …)` head names the binding in a pattern, which
/// [`crate::binding_scope::Declarations`] counts as a second declaration, so
/// such a binding never reaches [`Reader::let_makers`].
struct LetUses<'a> {
    key: &'a BindingKey,
    claimed: &'a HashSet<u32>,
    unclaimed: bool,
    returns: usize,
}

impl Visit for LetUses<'_> {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        // [`LetFlow`] claims only a plain `=`.
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(target)) = &assign.left
            && ident_key(&target.id) == *self.key
            && !self.claimed.contains(&assign.span.lo.0)
        {
            self.unclaimed = true;
        }
        assign.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        if names_binding(&update.arg, self.key) {
            self.unclaimed = true;
        }
        update.visit_children_with(self);
    }

    // A nested function's `return binding` is counted too: the walk never
    // reaches it, so the binding is read as nothing. Read or not, a closure
    // that returns it hands it on ([`super::BindingUse::returned_at`]).
    fn visit_return_stmt(&mut self, ret: &ReturnStmt) {
        if ret
            .arg
            .as_deref()
            .is_some_and(|arg| names_binding(arg, self.key))
        {
            self.returns += 1;
        }
        ret.visit_children_with(self);
    }
}

/// The writes of a module `let` the lazy-client rule reads, anywhere in the
/// module (carrick#1790, [`Reader::lazy_clients`]): what each statement `x =
/// …`, `x ??= …` or `x ||= …` sets it to, and whether `x++` or `x--` writes
/// it. Any other write of the name (another operator, or one whose value is
/// used: `register(x = …)`, `return (x = …)`) is among the assignments
/// [`super::Reassigned`] counts and not among these, which leaves the `let`
/// no client.
struct LazyWrites<'a> {
    key: &'a BindingKey,
    writes: Vec<Expr>,
    updated: bool,
}

impl Visit for LazyWrites<'_> {
    fn visit_expr_stmt(&mut self, stmt: &ExprStmt) {
        if let Expr::Assign(assign) = unwrap_expression(&stmt.expr)
            && matches!(
                assign.op,
                AssignOp::Assign | AssignOp::NullishAssign | AssignOp::OrAssign
            )
            && let AssignTarget::Simple(SimpleAssignTarget::Ident(target)) = &assign.left
            && ident_key(&target.id) == *self.key
        {
            self.writes.push((*assign.right).clone());
        }
        stmt.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        if names_binding(&update.arg, self.key) {
            self.updated = true;
        }
        update.visit_children_with(self);
    }
}

/// Whether a module `let`'s initialiser leaves it holding nothing (carrick#1790):
/// `null` or `undefined`.
fn holds_nothing(init: &Expr) -> bool {
    match unwrap_expression(init) {
        Expr::Lit(Lit::Null(_)) => true,
        Expr::Ident(ident) => ident.sym == *"undefined",
        _ => false,
    }
}

/// Whether a module `let`'s uses across its file are only tests, comparisons
/// and returns (carrick#1790): nothing is called, constructed or read
/// through it, and it is handed nowhere, spread, written through or
/// exported.
fn only_tested_and_returned(used: &BindingUse) -> bool {
    !used.written
        && !used.member_read
        && !used.spread
        && !used.other
        && !used.called_computed
        && !used.exported
        && used.called.is_empty()
        && used.library_calls.is_empty()
}

/// Whether `expr` is the binding `key`, through parentheses and type
/// assertions.
fn names_binding(expr: &Expr, key: &BindingKey) -> bool {
    matches!(unwrap_expression(expr), Expr::Ident(ident) if ident_key(ident) == *key)
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

/// Whether two instances are one maker's, handed the same arguments: the
/// same export's maker, or a call to the same binding (carrick#1562),
/// awaited alike.
pub(super) fn same_maker(a: &ClientRef, b: &ClientRef) -> bool {
    a.package == b.package
        && a.export == b.export
        && match (&a.instance, &b.instance) {
            (Some(a), Some(b)) => {
                a.form == b.form
                    && a.member == b.member
                    && a.args == b.args
                    && a.made_by == b.made_by
                    && a.awaited == b.awaited
            }
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
    let (text, holes) = split_text(text_pieces(expr, scope));
    SiteArg {
        text,
        object: match expr {
            Expr::Object(object) => Some(site_object(object, scope)),
            _ => None,
        },
        function: matches!(expr, Expr::Arrow(_) | Expr::Fn(_)),
        holes,
    }
}

/// Text pieces as literal text when no parameter is among them, and as
/// holes otherwise.
fn split_text(pieces: Option<Vec<TextPiece>>) -> (Option<String>, Option<Vec<TextPiece>>) {
    match pieces {
        Some(pieces)
            if pieces
                .iter()
                .all(|piece| matches!(piece, TextPiece::Lit(_))) =>
        {
            (Some(joined(&pieces)), None)
        }
        holes => (None, holes),
    }
}

/// Literal pieces as one text.
fn joined(pieces: &[TextPiece]) -> String {
    pieces
        .iter()
        .map(|piece| match piece {
            TextPiece::Lit(text) => text.as_str(),
            TextPiece::Param(_) | TextPiece::Named(_) => "",
        })
        .collect()
}

/// An object literal's keys ([`SiteObject`]). A spread or a computed key can
/// overwrite what came before it, so every earlier key is dropped and the
/// object is open; a key written after it is known.
fn site_object(object: &ObjectLit, scope: &Scope<'_>) -> SiteObject {
    let mut out = SiteObject::default();
    let open = |out: &mut SiteObject| {
        out.fields.clear();
        out.holes.clear();
        out.open = true;
    };
    let put = |out: &mut SiteObject, key: String, pieces: Option<Vec<TextPiece>>| {
        let (text, holes) = split_text(pieces);
        match holes {
            Some(holes) => out.holes.insert(key.clone(), holes),
            None => out.holes.remove(&key),
        };
        out.fields.insert(key, text);
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
                Some(key) => put(&mut out, key, text_pieces(&kv.value, scope)),
                None => open(&mut out),
            },
            Prop::Shorthand(ident) => put(
                &mut out,
                ident.sym.to_string(),
                text_pieces(&Expr::Ident(ident.clone()), scope),
            ),
            Prop::Method(MethodProp { key, .. })
            | Prop::Getter(GetterProp { key, .. })
            | Prop::Setter(SetterProp { key, .. }) => match prop_name(key) {
                Some(key) => put(&mut out, key, None),
                None => open(&mut out),
            },
            Prop::Assign(_) => open(&mut out),
        }
    }
    out
}

/// The text `expr` is, when the source states it here: [`text_pieces`] with
/// no hole in it. A parameter, and anything read through a module-scope
/// binding (an entry of a constant object, an imported constant, a
/// builder's return), is a hole until a caller or the link step fills it; a
/// binding a nested block declares, any other call and any operator but `+`
/// are never text.
///
/// The one place a name is read, with [`text_pieces`].
pub(super) fn literal_text(expr: &Expr, scope: &Scope<'_>) -> Option<String> {
    match split_text(text_pieces(expr, scope)) {
        (Some(text), _) => Some(text),
        (None, _) => None,
    }
}

/// The text `expr` is as [`literal_text`] reads it, with two kinds of hole
/// (carrick#1562):
///
/// - a parameter of the function it is written in: the parameter itself, or
///   a template, a `+` or a settled local built from it. A caller that
///   passes literal text fills it; nothing else does.
/// - a module-scope binding of the file, or an import, that is not literal
///   text here: an entry of a constant object (`TOPICS.orders`), a builder
///   called with text (`topicFor("created")`, `ENDPOINTS.users.byId(id)`),
///   or an imported constant. It is read once every module's uses of the
///   binding are known ([`LinkedNames`]).
///
/// Every hop of an entry's path is a plain name, and a builder's every
/// argument is text, or nothing is read.
pub(super) fn text_pieces(expr: &Expr, scope: &Scope<'_>) -> Option<Vec<TextPiece>> {
    match unwrap_expression(expr) {
        Expr::Lit(Lit::Str(text)) => Some(vec![TextPiece::Lit(text.value.to_string())]),
        Expr::Tpl(tpl) => {
            let mut out = Vec::new();
            for (index, quasi) in tpl.quasis.iter().enumerate() {
                let text = match &quasi.cooked {
                    Some(cooked) => cooked.to_string(),
                    None => quasi.raw.to_string(),
                };
                push_pieces(&mut out, vec![TextPiece::Lit(text)]);
                if let Some(hole) = tpl.exprs.get(index) {
                    push_pieces(&mut out, text_pieces(hole, scope)?);
                }
            }
            Some(out)
        }
        Expr::Bin(bin) if bin.op == BinaryOp::Add => {
            let mut out = text_pieces(&bin.left, scope)?;
            push_pieces(&mut out, text_pieces(&bin.right, scope)?);
            Some(out)
        }
        Expr::Ident(ident) => {
            if let Some(index) = scope.param_index(ident) {
                return Some(vec![TextPiece::Param(index)]);
            }
            scope
                .text(ident)
                .or_else(|| Some(vec![named(scope.named_binding(ident)?, Vec::new(), None)]))
        }
        Expr::Member(member) => {
            let (root, path) = named_path(member)?;
            Some(vec![named(scope.named_binding(root)?, path, None)])
        }
        Expr::Call(call) => {
            let Callee::Expr(callee) = &call.callee else {
                return None;
            };
            let (root, path) = match unwrap_expression(callee) {
                Expr::Ident(ident) => (ident, Vec::new()),
                Expr::Member(member) => named_path(member)?,
                _ => return None,
            };
            let binding = scope.named_binding(root)?;
            let mut args = Vec::with_capacity(call.args.len());
            for arg in &call.args {
                if arg.spread.is_some() {
                    return None;
                }
                args.push(text_pieces(&arg.expr, scope)?);
            }
            Some(vec![named(binding, path, Some(args))])
        }
        _ => None,
    }
}

fn named(binding: String, path: Vec<String>, args: Option<Vec<Vec<TextPiece>>>) -> TextPiece {
    TextPiece::Named(Box::new(NamedRef {
        binding,
        path,
        args,
    }))
}

/// `root.a.b`, every hop a plain name: the root and the hops in order.
pub(super) fn named_path(member: &MemberExpr) -> Option<(&Ident, Vec<String>)> {
    let mut path = vec![member_prop(member)?];
    let mut at = unwrap_expression(&member.obj);
    loop {
        match at {
            Expr::Ident(root) => {
                path.reverse();
                return Some((root, path));
            }
            Expr::Member(inner) => {
                path.push(member_prop(inner)?);
                at = unwrap_expression(&inner.obj);
            }
            _ => return None,
        }
    }
}

/// Append `pieces` to `out`, joining adjacent literal text.
pub(super) fn push_pieces(out: &mut Vec<TextPiece>, pieces: Vec<TextPiece>) {
    for piece in pieces {
        match (out.last_mut(), piece) {
            (Some(TextPiece::Lit(prev)), TextPiece::Lit(next)) => prev.push_str(&next),
            (_, TextPiece::Lit(next)) if next.is_empty() => {}
            (_, piece) => out.push(piece),
        }
    }
}

impl Scope<'_> {
    /// The text the binding `ident` resolves to holds, by its key
    /// ([`crate::binding_scope`]): never another binding of the same name.
    fn text(&self, ident: &Ident) -> Option<Vec<TextPiece>> {
        if self.param_index(ident).is_some() {
            return None;
        }
        let key = ident_key(ident);
        self.texts
            .get(&key)
            .or_else(|| self.module.texts.get(&key))
            .cloned()
    }

    /// The module-scope binding `ident` names, when a name may be read
    /// through it at link time (carrick#1562): one of the module's own
    /// constant objects or builders, by its key, or an import no scope
    /// below declares again. A parameter or a local of the same name is
    /// neither.
    fn named_binding(&self, ident: &Ident) -> Option<String> {
        if self.param_index(ident).is_some() {
            return None;
        }
        let key = ident_key(ident);
        if self.locals.contains_key(&key) || self.texts.contains_key(&key) {
            return None;
        }
        let name = ident.sym.as_ref();
        (self.module.named.contains(&key) || self.module.imports.contains(name))
            .then(|| name.to_string())
    }
}

/// What a module-scope `const` (or function) initialiser holds as a name
/// reads it ([`NameValue`]), in the module's scope (carrick#1562): text, an
/// object literal whose every entry is text, another such object or a
/// builder, or a builder: an arrow or a function whose body only returns
/// text built from its parameters. An object with a spread, a computed key,
/// a method or an accessor may hold, or be handed, anything: it is none.
pub(super) fn name_value(expr: &Expr, module: &ModuleScope) -> Option<NameValue> {
    match unwrap_expression(expr) {
        Expr::Object(object) => {
            let mut entries = BTreeMap::new();
            for prop in &object.props {
                let PropOrSpread::Prop(prop) = prop else {
                    return None;
                };
                let (key, value) = match &**prop {
                    Prop::KeyValue(kv) => (prop_name(&kv.key)?, &*kv.value),
                    _ => return None,
                };
                if let Some(value) = name_value(value, module) {
                    entries.insert(key, value);
                }
            }
            Some(NameValue::Map(entries))
        }
        Expr::Arrow(_) | Expr::Fn(_) => builder(&super::Builder::of(expr)?, module),
        other => Some(NameValue::Text(text_pieces(other, &Scope::module(module))?)),
    }
}

/// What a builder ([`super::Builder`]) returns as a name reads it
/// ([`NameValue::Builder`]): text built from its parameters.
pub(super) fn builder(builder: &super::Builder, module: &ModuleScope) -> Option<NameValue> {
    let mut scope = Scope::module(module);
    scope.params = builder.params.clone();
    Some(NameValue::Builder(text_pieces(&builder.returned, &scope)?))
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
    const MANIFEST: &str = "{ \"name\": \"service\", \"dependencies\": { \"@fixture/jobs\": \"^1.0.0\", \"@fixture/queue\": \"^2.0.0\", \"@fixture/bus\": \"^1.0.0\", \"fixture-bus\": \"^1.0.0\", \"@fixture/socket\": \"^1.0.0\" } }\n";

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

    /// The one maker of an instance site.
    fn maker(site: &LibrarySite) -> &SiteMaker {
        match &site.receiver {
            SiteReceiver::Instance(makers) if makers.len() == 1 => &makers[0],
            _ => panic!("expected an instance of one maker: {site:#?}"),
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
        assert_eq!(trigger.receiver_ids(), ["instance:()"]);
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
        assert_eq!(add.receiver_ids(), ["instance:new"]);
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
        assert!(trigger.selected_by(
            None,
            &Selector {
                on: Some(On::Export),
                of: None,
                path: &path,
                member: Some("trigger"),
            }
        ));
        assert!(!trigger.selected_by(
            None,
            &Selector {
                on: Some(On::Export),
                of: None,
                path: &[],
                member: Some("trigger"),
            }
        ));
    }

    /// `on` picks the export or its instances, and `of` exactly one receiver
    /// by its receiver id; an element carrying both, or neither, picks
    /// nothing (contract amendment 2, B3).
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
        let on = |on: On, member: &'static str| Selector {
            on: Some(on),
            of: None,
            path: &[],
            member: Some(member),
        };
        let of = |of: &'static str, member: &'static str| Selector {
            on: None,
            of: Some(of),
            path: &[],
            member: Some(member),
        };

        let on_task = site(&sites, "src/jobs.ts", 5, Some("trigger"));
        assert_eq!(on_task.receiver_ids(), ["instance:task"]);
        let task = Some(maker(on_task));
        assert!(on_task.selected_by(task, &of("instance:task", "trigger")));
        assert!(!on_task.selected_by(task, &of("instance:stream", "trigger")));
        assert!(
            !on_task.selected_by(task, &of("task", "trigger")),
            "`of` is a receiver id, not a maker's member"
        );
        assert!(on_task.selected_by(task, &on(On::Instance, "trigger")));
        assert!(on_task.selected_by(task, &on(On::Both, "trigger")));
        assert!(!on_task.selected_by(task, &on(On::Export, "trigger")));
        assert!(
            !on_task.selected_by(
                task,
                &Selector {
                    on: Some(On::Instance),
                    of: Some("instance:task"),
                    path: &[],
                    member: Some("trigger"),
                }
            ),
            "`on` and `of` together select nothing"
        );
        assert!(
            !on_task.selected_by(
                task,
                &Selector {
                    on: None,
                    of: None,
                    path: &[],
                    member: Some("trigger"),
                }
            ),
            "neither selects nothing"
        );
        assert!(
            !on_task.selected_by(None, &on(On::Both, "trigger")),
            "an instance site's call is not made through the export"
        );

        let on_stream = site(&sites, "src/jobs.ts", 6, Some("append"));
        assert_eq!(on_stream.receiver_ids(), ["instance:stream"]);
        assert!(!on_stream.selected_by(Some(maker(on_stream)), &of("instance:task", "append")));
        assert!(
            !on_stream.selected_by(task, &on(On::Instance, "append")),
            "a maker of another site is none of this one's"
        );

        let on_export = site(&sites, "src/jobs.ts", 7, Some("trigger"));
        assert!(on_export.selected_by(None, &on(On::Export, "trigger")));
        assert!(on_export.selected_by(None, &on(On::Both, "trigger")));
        assert!(!on_export.selected_by(None, &of("instance:task", "trigger")));
        assert!(!on_export.selected_by(None, &on(On::Instance, "trigger")));
        assert!(!on_export.selected_by(task, &on(On::Both, "trigger")));
    }

    /// A maker's Slots read what the maker was handed (contract amendment 2,
    /// B2): a positional name, and a key of an options object.
    #[test]
    fn a_maker_slot_reads_what_the_maker_was_handed() {
        let sites = sites_of(&[(
            "src/queues.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             const emails = new Queue(\"emails\", { prefix: \"app\" });\n\
             export async function go() { await emails.add(\"welcome\", {}); }\n",
        )]);
        let add = site(&sites, "src/queues.ts", 3, Some("add"));
        let made = maker(add);
        assert_eq!(made.literal(0, None), Some("emails"));
        assert_eq!(made.literal(1, Some("prefix")), Some("app"));
        assert_eq!(made.literal(1, Some("name")), None);
        assert!(made.supplies(1, Some("prefix")));
        assert!(!made.supplies(2, None));
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
    /// constants. A binding assigned again is not a literal here; an entry
    /// of a constant object is (carrick#1562).
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
            name(13).as_deref(),
            Some("order.shipped"),
            "an entry of a constant object (carrick#1562)"
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
            (
                "a return in another module (carrick#1562)",
                "import { emails } from \"./queue\";\n\
                 export function get() { return emails; }\n",
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

    /// A member read whose value is only tested keeps nothing of the
    /// receiver, so it contests nothing (carrick#1690): the test of an `if`,
    /// a loop or a conditional, the operand of `!`, a comparison operand, and
    /// an operand of a `&&` or `||` that is itself tested, the binding itself
    /// included. Read as a value anywhere else (assigned, passed, a
    /// property's value, returned, in an array or a template, an operand of a
    /// `||` or `??` whose value is used, a conditional's branch), it still
    /// contests. Each case is a service of its own, so no use of one masks
    /// another's.
    #[test]
    fn a_member_read_only_tested_contests_no_message_role() {
        let log = "export function log(value: unknown) {}\n";
        let module_const = |statement: &str| {
            let source = format!(
                "import {{ Queue }} from \"@fixture/queue\";\n\
                 import {{ log }} from \"./log\";\n\
                 const emails = new Queue(\"emails\");\n\
                 export async function welcome() {{ await emails.add(\"welcome\", {{}}); }}\n\
                 export function check(flag: boolean, retrying: boolean) {{ {statement} }}\n"
            );
            let sites = sites_of(&[("src/mail.ts", source.as_str()), ("src/log.ts", log)]);
            site(&sites, "src/mail.ts", 4, Some("add")).contest(on_wire)
        };
        for statement in [
            "if (emails.closing) { return 1; }",
            "while (emails.paused) { break; }",
            "do { break; } while (emails.paused);",
            "for (; emails.paused; ) { break; }",
            "return emails.ready ? 1 : 2;",
            "return !emails.ready;",
            "return emails.count > 3;",
            "if (emails.recovered || retrying) { return 1; }",
            "if (!(flag && emails.a.b)) { return 1; }",
            "if ((emails as any)?.ready) { return 1; }",
            "if (emails && emails.ready) { return 1; }",
        ] {
            assert_eq!(module_const(statement), None, "{statement}");
        }
        for statement in [
            "const opts = emails.opts; return opts;",
            "log(emails.opts);",
            "log({ active: emails.active });",
            "return emails.opts;",
            "return [emails.opts];",
            "return `${emails.name}`;",
            "return emails.opts || {};",
            "if (emails.opts ?? flag) { return 1; }",
            "return flag ? emails.opts : null;",
        ] {
            assert_eq!(module_const(statement), Some(Contest::Used), "{statement}");
        }

        // An own factory's socket, tested in its own callback and read
        // through the factory by a field (the shape carrick#1690 found), and
        // the same factory with the read also passed to a logger.
        let factory = |extra: &str| {
            let source = format!(
                "import {{ Socket }} from \"@fixture/socket\";\n\
                 import {{ log }} from \"./log\";\n\
                 let reconnecting = false;\n\
                 export class Supervisor {{\n\
                 \x20 private socket?: Socket;\n\
                 \x20 start() {{ this.socket = this.createSocket(); }}\n\
                 \x20 createSocket() {{\n\
                 \x20   const socket = new Socket(\"ws://dev\");\n\
                 \x20   socket.on(\"connect\", () => {{ if (socket.recovered || reconnecting) {{ log(\"back\"); }} }});\n\
                 \x20   {extra}\n\
                 \x20   return socket;\n\
                 \x20 }}\n\
                 \x20 subscribe() {{ if (!this.socket) {{ return; }} this.socket.emit(\"run:subscribe\", {{}}); }}\n\
                 }}\n"
            );
            let sites = sites_of(&[("src/supervisor.ts", source.as_str()), ("src/log.ts", log)]);
            let emit = site(&sites, "src/supervisor.ts", 13, Some("emit"));
            assert_eq!(maker(emit).holder, Holder::Field, "{extra}");
            emit.contest(on_wire)
        };
        assert_eq!(factory(""), None);
        assert_eq!(
            factory("socket.on(\"disconnect\", () => log({ active: socket.active }));"),
            Some(Contest::Used)
        );

        // A field read only in a test.
        let sites = sites_of(&[(
            "src/worker.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             export class Worker {\n\
             \x20 private queue = new Queue(\"work\");\n\
             \x20 run() { if (!this.queue.closing) { return this.queue.add(\"tick\", {}); } }\n\
             }\n",
        )]);
        assert_eq!(
            site(&sites, "src/worker.ts", 4, Some("add")).contest(on_wire),
            None
        );
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
        assert_eq!(field.receiver_ids(), ["instance:new"]);
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
        assert_eq!(in_method.receiver_ids(), ["instance:new"]);
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

    /// The service's own queue factory, for the factory tests (carrick#1562).
    const QUEUE_FACTORY: &str = "import { Queue } from \"@fixture/queue\";\n\
         export function createQueue(name: string) {\n\
         \x20 const queue = new Queue(name);\n\
         \x20 queue.on(\"error\", () => {});\n\
         \x20 return queue;\n\
         }\n\
         export async function connectQueue(name: string) {\n\
         \x20 return new Queue(`${name}.v2`);\n\
         }\n\
         export function namedQueue(name: string) {\n\
         \x20 return createQueue(name);\n\
         }\n\
         export const arrowQueue = (name: string) => new Queue(name);\n";

    /// The one site at `line` of `file` that calls `member` through an
    /// instance, with its maker.
    fn through<'s>(
        sites: &'s [LibrarySite],
        file: &str,
        line: u32,
        member: &str,
    ) -> (&'s LibrarySite, &'s SiteMaker) {
        let found = site(sites, file, line, Some(member));
        (found, maker(found))
    }

    /// An instance a function of the service returns is read at every call
    /// that holds it (carrick#1562): in a module `const` (and in the modules
    /// that import it), a function's `const`, a class field, and a call made
    /// on the returned instance itself. The maker is the factory's, handed
    /// what the caller passed; the factory's own calls through its binding
    /// count among the uses; an `async` factory is read through `await`; a
    /// factory that returns another's instance is followed to it.
    #[test]
    fn an_instance_an_own_factory_returns_is_read_where_it_is_held() {
        let sites = sites_of(&[
            ("src/queues.ts", QUEUE_FACTORY),
            (
                "src/use.ts",
                "import { createQueue, connectQueue, namedQueue, arrowQueue } from \"./queues\";\n\
                 export const emails = createQueue(\"emails\");\n\
                 export async function local() {\n\
                 \x20 const jobs = createQueue(\"jobs\");\n\
                 \x20 await jobs.add(\"tick\", {});\n\
                 }\n\
                 export async function chained() {\n\
                 \x20 await createQueue(\"chained\").add(\"once\", {});\n\
                 }\n\
                 export async function awaited() {\n\
                 \x20 const late = await connectQueue(\"late\");\n\
                 \x20 await late.add(\"ping\", {});\n\
                 }\n\
                 export class Mailer {\n\
                 \x20 private q = createQueue(\"mailer\");\n\
                 \x20 send() { return this.q.add(\"welcome\", {}); }\n\
                 }\n\
                 export async function welcome() { await emails.add(\"welcome\", {}); }\n\
                 export async function named() { await namedQueue(\"named\").add(\"x\", {}); }\n\
                 export async function arrow() { await arrowQueue(\"arrow\").add(\"a\", {}); }\n",
            ),
            (
                "src/other.ts",
                "import { emails } from \"./use\";\n\
                 export function again() { return emails.add(\"again\", {}); }\n",
            ),
        ]);
        let on_error = MemberUse {
            form: MakerForm::Call,
            path: Vec::new(),
            member: Some("on".to_string()),
        };
        for (file, line, name, holder, called_at) in [
            ("src/use.ts", 5, "jobs", Holder::Local, 4),
            ("src/use.ts", 8, "chained", Holder::Chained, 8),
            ("src/use.ts", 16, "mailer", Holder::Field, 15),
            ("src/use.ts", 18, "emails", Holder::Module, 2),
            ("src/other.ts", 2, "emails", Holder::Module, 2),
            ("src/use.ts", 19, "named", Holder::Chained, 19),
        ] {
            let (add, made) = through(&sites, file, line, "add");
            assert_eq!(add.receiver_ids(), ["instance:new"], "{file}:{line}");
            assert_eq!(add.specifier, "@fixture/queue", "{file}:{line}");
            assert_eq!(add.export, "Queue", "{file}:{line}");
            assert!(made.file.ends_with("src/queues.ts"), "{file}:{line}");
            assert_eq!(made.line, 3, "the factory's maker: {file}:{line}");
            assert_eq!(made.args[0].text.as_deref(), Some(name), "{file}:{line}");
            assert_eq!(made.holder, holder, "{file}:{line}");
            let factory = made.factory.as_ref().expect("the own call");
            assert!(factory.file.ends_with("src/use.ts"), "{file}:{line}");
            assert_eq!(factory.line, called_at, "{file}:{line}");
            assert!(add.uses.contains(&on_error), "{file}:{line}: {add:#?}");
            assert_eq!(add.contest(on_wire), None, "{file}:{line}");
        }

        let (late, made) = through(&sites, "src/use.ts", 12, "add");
        assert_eq!(made.line, 8, "the async factory's maker");
        assert_eq!(made.args[0].text.as_deref(), Some("late.v2"));
        assert_eq!(late.contest(on_wire), None);

        let (arrow, made) = through(&sites, "src/use.ts", 20, "add");
        assert_eq!(
            made.line, 13,
            "an arrow's expression body is what it returns"
        );
        assert_eq!(made.args[0].text.as_deref(), Some("arrow"));
        assert_eq!(arrow.contest(on_wire), None);

        // The factory's own call through the binding it returns is a hand-off
        // to every caller, as any return is.
        let inside = site(&sites, "src/queues.ts", 4, Some("on"));
        assert_eq!(inside.contest(on_wire), Some(Contest::Used));

        // The factory's maker takes its name from a parameter, so it is a
        // maker again at every call of the factory, with the name there.
        let maker_site = |file: &str, line: u32| {
            sites
                .iter()
                .find(|site| {
                    site.file.ends_with(file)
                        && site.line == line
                        && site.makes(MakerForm::New, None)
                })
                .unwrap_or_else(|| panic!("a maker at {file}:{line}: {sites:#?}"))
        };
        assert_eq!(maker_site("src/queues.ts", 3).literal(0, None), None);
        let defined = maker_site("src/use.ts", 2);
        assert_eq!(defined.literal(0, None), Some("emails"));
        let origin = defined
            .origin
            .as_ref()
            .expect("filled by the factory's caller");
        assert!(origin.file.ends_with("src/queues.ts"));
        assert_eq!(origin.line, 3);
        // A call made on what the factory returns starts where the factory
        // call does: only the factory call fills the factory's maker.
        let chained: Vec<Option<&str>> = sites
            .iter()
            .filter(|site| {
                site.file.ends_with("src/use.ts")
                    && site.line == 8
                    && site.makes(MakerForm::New, None)
            })
            .map(|site| site.literal(0, None))
            .collect();
        assert_eq!(chained, vec![Some("chained")], "{sites:#?}");
    }

    /// An export's binding returned from a function hands it to whoever
    /// calls that function, which may configure what its makers build, so
    /// every instance the module makes from it is taken away, as when the
    /// return was an operand before carrick#1562 split it out.
    #[test]
    fn a_returned_export_takes_its_instances_away() {
        let source = |extra: &str| {
            format!(
                "import {{ Queue }} from \"@fixture/queue\";\n\
                 const emails = new Queue(\"emails\");\n\
                 export function send() {{ return emails.add(\"welcome\", {{}}); }}\n\
                 {extra}"
            )
        };
        let control = source("");
        let sites = sites_of(&[("src/mail.ts", control.as_str())]);
        assert_eq!(
            site(&sites, "src/mail.ts", 3, Some("add")).contest(on_wire),
            None
        );
        let returned = source("export function queueClass() { return Queue; }\n");
        let sites = sites_of(&[("src/mail.ts", returned.as_str())]);
        assert_eq!(
            site(&sites, "src/mail.ts", 3, Some("add")).contest(on_wire),
            Some(Contest::Used)
        );
    }

    /// A call made on what a package's maker returns, with no binding in
    /// between, is no receiver form the contract names: only one made on what
    /// an own factory returns is a site (carrick#1562).
    #[test]
    fn a_call_on_what_a_package_maker_returns_is_no_site() {
        let sites = sites_of(&[
            ("src/queues.ts", QUEUE_FACTORY),
            (
                "src/use.ts",
                "import { Queue } from \"@fixture/queue\";\n\
                 import { createQueue } from \"./queues\";\n\
                 export async function go() {\n\
                 \x20 await new Queue(\"temp\").add(\"t\", {});\n\
                 \x20 await createQueue(\"own\").add(\"o\", {});\n\
                 }\n",
            ),
        ]);
        let adds: Vec<u32> = sites
            .iter()
            .filter(|site| {
                site.file.ends_with("src/use.ts") && site.member.as_deref() == Some("add")
            })
            .map(|site| site.line)
            .collect();
        assert_eq!(adds, vec![5], "{sites:#?}");
    }

    /// A module `require` loads is the package it names, never a function's
    /// return (carrick#1562): the binding is the import, and its calls are
    /// sites through the export.
    #[test]
    fn a_module_a_require_loads_is_no_own_call() {
        let sites = sites_of(&[(
            "src/legacy.ts",
            "const jobs = require(\"@fixture/jobs\");\n\
             export function run() { return jobs.trigger(\"nightly\", {}); }\n",
        )]);
        let trigger = site(&sites, "src/legacy.ts", 2, Some("trigger"));
        assert_eq!(trigger.receiver, SiteReceiver::Export);
        assert_eq!(trigger.specifier, "@fixture/jobs");
        assert_eq!(trigger.literal(0, None), Some("nightly"));
    }

    /// What is not a fresh instance is no factory's (carrick#1562), and a
    /// call holding it reads nothing: a module's instance or a field, which
    /// every call shares; a path that returns something else; a generator;
    /// an `async` factory called without `await`; makers of two exports on
    /// two paths (carrick#1689). A factory that hands its instance on, or
    /// returns it from anywhere but its own body, leaves the instance taken
    /// away. Uses are kept by name across a file, so each factory names its
    /// binding apart from every other. One maker handed other arguments on
    /// two paths, and a binding set on each path, are read since
    /// carrick#1689, as a set of makers.
    #[test]
    fn what_is_not_one_fresh_instance_is_no_factory_s() {
        let factories = "import { Queue, Worker } from \"@fixture/queue\";\n\
             import { register } from \"./registry\";\n\
             export function perBranch(flag: boolean) { if (flag) { return new Queue(\"a\"); } return new Worker(\"a\"); }\n\
             export function perArgs(flag: boolean) { if (flag) { return new Queue(\"a\"); } return new Queue(\"b\"); }\n\
             export function assigned(flag: boolean) { let picked; if (flag) { picked = new Queue(\"a\"); } else { picked = new Queue(\"a\"); } return picked; }\n\
             const shared = new Queue(\"shared\");\n\
             export function getShared() { return shared; }\n\
             export function sendShared() { return shared.add(\"s\", {}); }\n\
             export function maybe(flag: boolean) { if (!flag) { return null; } return new Queue(\"m\"); }\n\
             export function* generated() { return new Queue(\"g\"); }\n\
             export async function later() { return new Queue(\"later\"); }\n\
             export function handedOff() { const handed = new Queue(\"h\"); register(handed); return handed; }\n\
             export function escapes() { const escaping = new Queue(\"e\"); register(() => escaping); return escaping; }\n\
             export function fresh() { return new Queue(\"control\"); }\n\
             export function other() { return new Queue(\"other\"); }\n";
        let use_of = |factory: &str| {
            format!(
                "import {{ {factory} }} from \"./factories\";\n\
                 export async function go() {{\n\
                 \x20 const q = {factory}(true);\n\
                 \x20 await q.add(\"tick\", {{}});\n\
                 }}\n"
            )
        };
        let sites_with = |factory: &str| {
            let user = use_of(factory);
            sites_of(&[
                ("src/factories.ts", factories),
                ("src/use.ts", user.as_str()),
                (
                    "src/registry.ts",
                    "export function register(value: unknown) {}\n",
                ),
            ])
        };
        let add_in_use = |sites: &[LibrarySite]| {
            sites
                .iter()
                .find(|site| site.file.ends_with("src/use.ts") && site.line == 4)
                .cloned()
        };

        let control = sites_with("fresh");
        let read = add_in_use(&control).expect("the control reads through its factory");
        assert_eq!(read.contest(on_wire), None);

        for factory in ["perBranch", "getShared", "maybe", "generated", "later"] {
            let sites = sites_with(factory);
            assert!(add_in_use(&sites).is_none(), "{factory}: {sites:#?}");
        }
        for (factory, makers) in [("perArgs", 2), ("assigned", 1)] {
            let sites = sites_with(factory);
            let add = add_in_use(&sites).expect(factory);
            assert_eq!(add.makers().len(), makers, "{factory} (carrick#1689)");
            assert_eq!(add.contest(on_wire), None, "{factory}");
        }
        for factory in ["handedOff", "escapes"] {
            let sites = sites_with(factory);
            let add = add_in_use(&sites).expect(factory);
            assert_eq!(add.contest(on_wire), Some(Contest::Used), "{factory}");
        }

        // A field two own factories write holds either one's instance, and
        // a parameter that shadows a factory's name calls no factory.
        let held = sites_of(&[
            ("src/factories.ts", factories),
            (
                "src/registry.ts",
                "export function register(value: unknown) {}\n",
            ),
            (
                "src/two.ts",
                "import { fresh, other } from \"./factories\";\n\
                 export class Mailer {\n\
                 \x20 private q = fresh();\n\
                 \x20 swap() { this.q = other(); }\n\
                 \x20 send() { return this.q.add(\"x\", {}); }\n\
                 }\n\
                 export function run(fresh: () => unknown) {\n\
                 \x20 const q = fresh();\n\
                 \x20 return (q as any).add(\"p\", {});\n\
                 }\n",
            ),
        ]);
        assert!(
            held.iter().all(|site| !site.file.ends_with("src/two.ts")),
            "{held:#?}"
        );
        assert_eq!(
            site(&control, "src/factories.ts", 8, Some("add")).contest(on_wire),
            Some(Contest::Used),
            "`return shared` hands the module's instance on"
        );
    }

    /// A getter that builds a client on first use into a module-scope `let`
    /// and returns it (carrick#1790) is read as an own factory of that
    /// maker's instance, at every call that holds what it returns: `if (!x)
    /// { x = … }`, `x ??= …`, a comparison with `null`, and a write of what
    /// another own factory returns.
    #[test]
    fn a_client_a_getter_builds_once_into_a_module_let_is_read_where_it_is_held() {
        let sites = sites_of(&[
            ("src/queues.ts", QUEUE_FACTORY),
            (
                "src/queue.ts",
                "import { Queue } from \"@fixture/queue\";\n\
                 import { createQueue } from \"./queues\";\n\
                 let lazy: Queue | null = null;\n\
                 export function getQueue(): Queue {\n\
                 \x20 if (!lazy) {\n\
                 \x20   lazy = new Queue(\"emails\");\n\
                 \x20 }\n\
                 \x20 return lazy!;\n\
                 }\n\
                 let nullish: Queue | undefined;\n\
                 export const getNullish = (): Queue => {\n\
                 \x20 nullish ??= new Queue(\"audit\");\n\
                 \x20 return nullish;\n\
                 };\n\
                 let owned: Queue | null = null;\n\
                 export function getOwned() {\n\
                 \x20 if (owned === null) {\n\
                 \x20   owned = createQueue(\"owned\");\n\
                 \x20 }\n\
                 \x20 return owned;\n\
                 }\n\
                 export async function send() {\n\
                 \x20 const q = getQueue();\n\
                 \x20 await q.add(\"welcome\", {});\n\
                 }\n",
            ),
            (
                "src/use.ts",
                "import { getQueue, getNullish, getOwned } from \"./queue\";\n\
                 export async function chained() {\n\
                 \x20 await getQueue().add(\"chained\", {});\n\
                 }\n\
                 export async function audited() {\n\
                 \x20 const audit = getNullish();\n\
                 \x20 await audit.add(\"entry\", {});\n\
                 }\n\
                 export async function ownedSend() {\n\
                 \x20 const o = getOwned();\n\
                 \x20 await o.add(\"tick\", {});\n\
                 }\n",
            ),
        ]);
        for (file, line, maker_file, maker_line, name, holder, called_at) in [
            (
                "src/queue.ts",
                24,
                "src/queue.ts",
                6,
                "emails",
                Holder::Local,
                23,
            ),
            (
                "src/use.ts",
                3,
                "src/queue.ts",
                6,
                "emails",
                Holder::Chained,
                3,
            ),
            (
                "src/use.ts",
                7,
                "src/queue.ts",
                12,
                "audit",
                Holder::Local,
                6,
            ),
            (
                "src/use.ts",
                11,
                "src/queues.ts",
                3,
                "owned",
                Holder::Local,
                10,
            ),
        ] {
            let (add, made) = through(&sites, file, line, "add");
            assert_eq!(add.receiver_ids(), ["instance:new"], "{file}:{line}");
            assert_eq!(add.specifier, "@fixture/queue", "{file}:{line}");
            assert_eq!(add.export, "Queue", "{file}:{line}");
            assert!(made.file.ends_with(maker_file), "{file}:{line}: {made:#?}");
            assert_eq!(made.line, maker_line, "{file}:{line}");
            assert_eq!(made.args[0].text.as_deref(), Some(name), "{file}:{line}");
            assert_eq!(made.holder, holder, "{file}:{line}");
            let factory = made.factory.as_ref().expect("the getter call");
            assert!(factory.file.ends_with(file), "{file}:{line}");
            assert_eq!(factory.line, called_at, "{file}:{line}");
            assert_eq!(add.contest(on_wire), None, "{file}:{line}: {add:#?}");
        }
        // Through another own factory, that factory's uses count too.
        let (owned, _) = through(&sites, "src/use.ts", 11, "add");
        assert!(owned.uses.contains(&MemberUse {
            form: MakerForm::Call,
            path: Vec::new(),
            member: Some("on".to_string()),
        }));
    }

    /// Every call of a getter that builds its client once holds one object,
    /// so every holder's uses are each holder's (carrick#1790): a member one
    /// holder calls is classified at every site through the client, a
    /// holder that hands it on takes it away everywhere, and so does a call
    /// of the getter that holds what it returns where no site reads it.
    #[test]
    fn every_holder_of_a_client_a_getter_builds_once_shares_its_uses() {
        let getter = |extra: &str| {
            format!(
                "import {{ Queue }} from \"@fixture/queue\";\n\
                 import {{ register }} from \"./registry\";\n\
                 let lazy: Queue | null = null;\n\
                 export function get() {{\n\
                 \x20 if (!lazy) {{\n\
                 \x20   lazy = new Queue(\"emails\");\n\
                 \x20 }}\n\
                 \x20 return lazy;\n\
                 }}\n\
                 export async function send() {{\n\
                 \x20 const held = get();\n\
                 \x20 await held.add(\"welcome\", {{}});\n\
                 }}\n\
                 {extra}"
            )
        };
        let send_in = |extra: &str| {
            let source = getter(extra);
            let sites = sites_of(&[
                ("src/queue.ts", source.as_str()),
                (
                    "src/registry.ts",
                    "export function register(value: unknown) {}\n",
                ),
            ]);
            site(&sites, "src/queue.ts", 12, Some("add")).clone()
        };
        let paused = MemberUse {
            form: MakerForm::Call,
            path: Vec::new(),
            member: Some("pause".to_string()),
        };
        let pause_unlisted = |_: &str, used: &MemberUse| {
            if used == &paused {
                MemberWire::Unlisted
            } else {
                MemberWire::OnWire
            }
        };

        let control = send_in("");
        assert_eq!(control.contest(pause_unlisted), None);

        let other = send_in("export function stop() { const other = get(); other.pause(); }\n");
        assert!(other.uses.contains(&paused), "{other:#?}");
        assert_eq!(
            other.contest(pause_unlisted),
            Some(Contest::Member {
                on: "instance:new".to_string(),
                used: paused.clone(),
                wire: MemberWire::Unlisted,
            })
        );

        for extra in [
            "export async function handed() { const out = get(); register(out); await out.add(\"x\", {}); }\n",
            "export function unread() { register(get()); }\n",
        ] {
            let send = send_in(extra);
            assert_eq!(send.contest(on_wire), Some(Contest::Used), "{extra}");
        }
    }

    /// A module `let` read any way but tested and returned, or set by
    /// anything but one maker handed the same arguments, is no lazily built
    /// client (carrick#1790): the getter returns no instance and the call
    /// holding it reads nothing. One returned from anywhere but the getter
    /// hands the instance on.
    #[test]
    fn what_may_be_set_or_read_otherwise_is_no_lazily_built_client() {
        let service = |declared: &str, extra: &str| {
            format!(
                "import {{ Queue }} from \"@fixture/queue\";\n\
                 import {{ register }} from \"./registry\";\n\
                 {declared}\n\
                 export function get() {{\n\
                 \x20 if (!lazy) {{\n\
                 \x20   lazy = new Queue(\"emails\");\n\
                 \x20 }}\n\
                 \x20 return lazy;\n\
                 }}\n\
                 export async function send() {{\n\
                 \x20 const held = get();\n\
                 \x20 await held.add(\"welcome\", {{}});\n\
                 }}\n\
                 {extra}\n"
            )
        };
        let send_in = |declared: &str, extra: &str| {
            let source = service(declared, extra);
            let sites = sites_of(&[
                ("src/queue.ts", source.as_str()),
                (
                    "src/registry.ts",
                    "export function register(value: unknown) {}\n",
                ),
            ]);
            sites
                .into_iter()
                .find(|site| site.file.ends_with("src/queue.ts") && site.line == 12)
        };
        let declared = "let lazy: Queue | null = null;";
        let control = send_in(declared, "").expect("the control reads through its getter");
        assert_eq!(control.contest(on_wire), None);

        for (declared, extra) in [
            (declared, "export function reset() { lazy = null; }"),
            ("export let lazy: Queue | null = null;", ""),
            (declared, "export { lazy };"),
            (declared, "register(lazy);"),
            (
                declared,
                "export function direct() { if (lazy) { lazy.pause(); } }",
            ),
            (
                declared,
                "export function sub() { if (lazy) { lazy.jobs.list(); } }",
            ),
            (
                declared,
                "export function keyed(k: string) { if (lazy) { lazy[k](); } }",
            ),
            (
                declared,
                "export function peek() { if (lazy) { register(lazy.name); } }",
            ),
            (
                declared,
                "export function poke() { if (lazy) { lazy.name = \"x\"; } }",
            ),
            (declared, "export function copy() { return { ...lazy }; }"),
            (
                declared,
                "export function swap() { lazy = new Queue(\"other\"); }",
            ),
            (
                declared,
                "export function shadow() { const lazy = 1; return lazy; }",
            ),
            (declared, "export function drop() { lazy &&= null; }"),
            (
                declared,
                "export function eager() { register(lazy = new Queue(\"emails\")); }",
            ),
            (
                declared,
                "export function unpack() { [lazy] = [new Queue(\"emails\")]; }",
            ),
            (
                declared,
                "export function each(all: Queue[]) { for (lazy of all) {} }",
            ),
            (
                "let lazy: any = null;",
                "export function bump() { lazy++; }",
            ),
            ("let lazy: Queue | null = new Queue(\"emails\");", ""),
            (
                "var lazy: Queue | null = null; var lazy: Queue | null = null;",
                "",
            ),
        ] {
            let send = send_in(declared, extra);
            assert!(send.is_none(), "{declared} {extra}: {send:#?}");
        }

        for extra in [
            "export function also() { return lazy; }",
            "export function later() { return () => lazy; }",
        ] {
            let send = send_in(declared, extra).expect(extra);
            assert_eq!(send.contest(on_wire), Some(Contest::Used), "{extra}");
        }

        // A getter that returns the `let` on one path and anything else on
        // another returns no one instance.
        let mixed = sites_of(&[(
            "src/mixed.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             let pick: Queue | null = null;\n\
             export function either(flag: boolean) {\n\
             \x20 if (flag) {\n\
             \x20   return new Queue(\"emails\");\n\
             \x20 }\n\
             \x20 if (!pick) {\n\
             \x20   pick = new Queue(\"emails\");\n\
             \x20 }\n\
             \x20 return pick;\n\
             }\n\
             export async function send() {\n\
             \x20 const chosen = either(true);\n\
             \x20 await chosen.add(\"x\", {});\n\
             }\n",
        )]);
        assert!(
            mixed
                .iter()
                .all(|site| !(site.file.ends_with("src/mixed.ts") && site.line == 14)),
            "{mixed:#?}"
        );

        // A `let` set to what another getter's `let` holds is one object
        // held through two `let`s, which nothing here joins.
        let nested = sites_of(&[(
            "src/nested.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             let inner: Queue | null = null;\n\
             export function getInner() {\n\
             \x20 if (!inner) {\n\
             \x20   inner = new Queue(\"emails\");\n\
             \x20 }\n\
             \x20 return inner;\n\
             }\n\
             let outer: Queue | null = null;\n\
             export function getOuter() {\n\
             \x20 if (!outer) {\n\
             \x20   outer = getInner();\n\
             \x20 }\n\
             \x20 return outer;\n\
             }\n\
             export async function send() {\n\
             \x20 const both = getOuter();\n\
             \x20 await both.add(\"x\", {});\n\
             }\n",
        )]);
        assert!(
            nested
                .iter()
                .all(|site| !(site.file.ends_with("src/nested.ts") && site.line == 18)),
            "{nested:#?}"
        );
    }

    /// A factory that returns a getter's one instance on one path and a
    /// fresh one of the same maker on another holds a set of two makers
    /// (carrick#1790): the getter's call inside the factory holds the shared
    /// instance where no site reads its uses, so the shared one is taken
    /// away, and the set with it.
    #[test]
    fn a_factory_that_returns_a_getter_s_instance_hands_it_on() {
        let sites = sites_of(&[(
            "src/either.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             let lazy: Queue | null = null;\n\
             export function get() {\n\
             \x20 if (!lazy) {\n\
             \x20   lazy = new Queue(\"emails\");\n\
             \x20 }\n\
             \x20 return lazy;\n\
             }\n\
             export function either(fresh: boolean) {\n\
             \x20 if (fresh) {\n\
             \x20   return new Queue(\"emails\");\n\
             \x20 }\n\
             \x20 return get();\n\
             }\n\
             export async function send() {\n\
             \x20 const picked = either(true);\n\
             \x20 await picked.add(\"x\", {});\n\
             }\n",
        )]);
        let add = site(&sites, "src/either.ts", 17, Some("add"));
        assert_eq!(add.makers().len(), 2, "{add:#?}");
        assert_eq!(add.contest(on_wire), Some(Contest::Used));
    }

    /// Own factories that build one of a few makers' instances, each binding
    /// named apart from every other (uses are kept by name across a file).
    const MAKER_SETS: &str = "import { Queue, Worker } from \"@fixture/queue\";\n\
         import { register } from \"./registry\";\n\
         export function createClient(flag: boolean) {\n\
         \x20 let client;\n\
         \x20 if (flag) {\n\
         \x20   client = new Queue.Cluster(\"events\", { cluster: true });\n\
         \x20 } else {\n\
         \x20   client = new Queue(\"events\");\n\
         \x20 }\n\
         \x20 client.on(\"error\", () => {});\n\
         \x20 return client;\n\
         }\n\
         export function byReturn(flag: boolean) { if (flag) { return new Queue.Cluster(\"events\"); } return new Queue(\"events\"); }\n\
         export function byArgs(flag: boolean) { if (flag) { return new Queue(\"a\"); } return new Queue(\"b\"); }\n\
         export function earlyThrow(flag: boolean) { let thrown; if (flag) { thrown = new Queue(\"t\"); } else { throw new Error(\"no\"); } return thrown; }\n\
         export function fourMakers(n: number) { if (n === 1) { return new Queue(\"m1\"); } if (n === 2) { return new Queue(\"m2\"); } if (n === 3) { return new Queue(\"m3\"); } return new Queue(\"m4\"); }\n\
         export function twoBindings(flag: boolean) { const first = new Queue(\"x\"); const second = new Queue(\"x\"); register(second); if (flag) { return first; } return second; }\n\
         export function handedLet(flag: boolean) { let handed; if (flag) { handed = new Queue(\"h1\"); } else { handed = new Queue(\"h2\"); } register(handed); return handed; }\n\
         export function unsetPath(flag: boolean) { let unset; if (flag) { unset = new Queue(\"u\"); } return unset; }\n\
         export function inLoop(items: string[]) { let looped = new Queue(\"l0\"); for (const item of items) { looped = new Queue(item); } return looped; }\n\
         export function inClosure() { let closed = new Queue(\"c0\"); const swap = () => { closed = new Queue(\"c1\"); }; swap(); return closed; }\n\
         export function compound() { let joined; joined ||= new Queue(\"j\"); return joined; }\n\
         export function nonInstance(flag: boolean) { let mixed; if (flag) { mixed = new Queue(\"o\"); } else { mixed = null; } return mixed; }\n\
         export function thirdPath(n: number) { if (n === 1) { return new Queue.Cluster(\"p\"); } if (n === 2) { return new Queue(\"p\"); } return null; }\n\
         export function tooMany(n: number) { if (n === 1) { return new Queue(\"n1\"); } if (n === 2) { return new Queue(\"n2\"); } if (n === 3) { return new Queue(\"n3\"); } if (n === 4) { return new Queue(\"n4\"); } return new Queue(\"n5\"); }\n\
         export function returnedEarly(flag: boolean) { let early; if (flag) { return early; } early = new Queue(\"e\"); return early; }\n\
         export function inTry() { let tried; try { tried = new Queue(\"t1\"); } catch { tried = new Queue(\"t2\"); } return tried; }\n\
         export function destructured() { let unpacked; [unpacked] = [new Queue(\"d\")]; return unpacked; }\n\
         export function twoExports(flag: boolean) { if (flag) { return new Queue(\"w\"); } return new Worker(\"w\"); }\n\
         export function helper() { return 42; }\n\
         export function ownMixed(flag: boolean) { if (flag) { return new Queue(\"om\"); } return helper(); }\n\
         export function threeA(n: number) { if (n === 1) { return new Queue(\"a1\"); } if (n === 2) { return new Queue(\"a2\"); } return new Queue(\"a3\"); }\n\
         export function threeB(n: number) { if (n === 1) { return new Queue(\"b1\"); } if (n === 2) { return new Queue(\"b2\"); } return new Queue(\"b3\"); }\n\
         export function nestedTooMany(n: number) { if (n > 3) { return threeA(n); } return threeB(n); }\n\
         export function plainA() { return new Queue(\"z\"); }\n\
         export function handA() { const handedA = new Queue(\"z\"); register(handedA); return handedA; }\n\
         export function eitherA(flag: boolean) { if (flag) { return plainA(); } return handA(); }\n\
         export function updated() { let counter: any = new Queue(\"c\"); counter++; return counter; }\n\
         export function returnInLoop(items: string[]) { let looping; for (const item of items) { return looping; } looping = new Queue(\"lr\"); return looping; }\n\
         export function nestedReturn(flag: boolean) { let inner; if (flag) { inner = new Queue(\"i1\"); } else { inner = new Queue(\"i2\"); } const get = () => { return inner; }; get(); return inner; }\n\
         export function exitByReturn(flag: boolean) { let exiting; if (flag) { exiting = new Queue.Cluster(\"x1\"); } else { return new Queue(\"x2\"); } return exiting; }\n\
         export function twiceVar(flag: boolean) { var dup = new Queue(\"v1\"); if (flag) { dup = new Queue(\"v2\"); } var dup = new Queue(\"v3\"); return dup; }\n\
         export function usesApart(flag: boolean) { const u1 = new Queue.Cluster(\"ua\"); u1.on(\"error\", () => {}); const u2 = new Queue(\"ua\"); u2.pause(); if (flag) { return u1; } return u2; }\n\
         export function handedSecond(flag: boolean) { if (flag) { return new Queue.Cluster(\"hs\"); } const hs = new Queue(\"hs\"); register(hs); return hs; }\n\
         export function crossModule(flag: boolean) { if (flag) { return new Queue(\"cm\"); } return otherQueue(); }\n\
         import { otherQueue } from \"./other\";\n";

    /// The sites of a service whose `src/use.ts` calls `factory(…)` and then
    /// `add` through what it returns, on line 4.
    fn through_set(factory: &str) -> Vec<LibrarySite> {
        let user = format!(
            "import {{ {factory} }} from \"./factories\";\n\
             export async function go() {{\n\
             \x20 const held = {factory}(1 as any);\n\
             \x20 await held.add(\"tick\", {{}});\n\
             }}\n"
        );
        sites_of(&[
            ("src/factories.ts", MAKER_SETS),
            ("src/use.ts", user.as_str()),
            (
                "src/registry.ts",
                "export function register(value: unknown) {}\n",
            ),
            (
                "src/other.ts",
                "import { Queue } from \"@fixture/queue\";\n\
                 Queue.defaults({ prefix: \"other\" });\n\
                 export function otherQueue() { return new Queue(\"oq\"); }\n",
            ),
        ])
    }

    fn add_through(sites: &[LibrarySite]) -> Option<&LibrarySite> {
        sites
            .iter()
            .find(|site| site.file.ends_with("src/use.ts") && site.line == 4)
    }

    /// An own factory that builds one of a few makers' instances, on every
    /// path, is read as the set of those makers (carrick#1689, contract
    /// amendment 3): a `let` set on each branch (the ticket's own shape), a
    /// return per branch, one maker handed other arguments on two paths, a
    /// branch that throws, and four makers. Each maker is read where it is
    /// written, in source order, and the factory's own uses of its binding
    /// are the set's. A `let` is read only where it is returned: no call is
    /// read through it, for any role.
    #[test]
    fn an_instance_of_one_of_a_few_makers_is_read_as_the_set() {
        let sites = through_set("createClient");
        let add = add_through(&sites).expect("a site through the let factory");
        assert_eq!(add.receiver_ids(), ["instance:new:Cluster", "instance:new"]);
        assert_eq!(add.specifier, "@fixture/queue");
        assert_eq!(add.export, "Queue");
        let lines: Vec<u32> = add.makers().iter().map(|maker| maker.line).collect();
        assert_eq!(lines, vec![6, 8]);
        for maker in add.makers() {
            assert_eq!(maker.args[0].text.as_deref(), Some("events"));
            assert_eq!(maker.holder, Holder::Local);
            assert_eq!(maker.factory.as_ref().map(|call| call.line), Some(3));
        }
        assert!(add.uses.contains(&MemberUse {
            form: MakerForm::Call,
            path: Vec::new(),
            member: Some("on".to_string()),
        }));
        assert_eq!(add.contest(on_wire), None);
        assert!(
            sites
                .iter()
                .all(|site| !(site.file.ends_with("src/factories.ts") && site.line == 10)),
            "no call is read through the let itself: {sites:#?}"
        );

        let by_return = through_set("byReturn");
        let add = add_through(&by_return).expect("byReturn");
        assert_eq!(add.receiver_ids(), ["instance:new:Cluster", "instance:new"]);

        let by_args = through_set("byArgs");
        let add = add_through(&by_args).expect("byArgs");
        assert_eq!(add.receiver_ids(), ["instance:new"], "one id, checked once");
        let names: Vec<Option<&str>> = add
            .makers()
            .iter()
            .map(|maker| maker.args[0].text.as_deref())
            .collect();
        assert_eq!(names, vec![Some("a"), Some("b")]);

        let thrown = through_set("earlyThrow");
        let add = add_through(&thrown).expect("a branch that throws returns nothing");
        assert_eq!(add.makers().len(), 1);

        let exits = through_set("exitByReturn");
        let add = add_through(&exits).expect("a branch that returns another instance");
        assert_eq!(add.receiver_ids(), ["instance:new", "instance:new:Cluster"]);

        let four = through_set("fourMakers");
        assert_eq!(add_through(&four).expect("fourMakers").makers().len(), 4);
    }

    /// A member use on an instance of a set is classified on every maker's
    /// receiver, and the set's uses are every binding's that holds one of
    /// its makers (carrick#1689).
    #[test]
    fn a_use_on_a_set_is_classified_on_every_maker() {
        let sites = through_set("createClient");
        let add = add_through(&sites).expect("createClient");
        let on_plain = |on: &str, used: &MemberUse| match (on, used.member.as_deref()) {
            ("instance:new", Some("on")) => MemberWire::Unlisted,
            _ => MemberWire::OnWire,
        };
        assert!(
            matches!(
                add.contest(on_plain),
                Some(Contest::Member { ref on, .. }) if on == "instance:new"
            ),
            "{add:#?}"
        );

        let sites = through_set("usesApart");
        let add = add_through(&sites).expect("usesApart");
        let pause = |_: &str, used: &MemberUse| match used.member.as_deref() {
            Some("pause") => MemberWire::ChangesName,
            _ => MemberWire::OnWire,
        };
        assert!(add.contest(pause).is_some(), "{add:#?}");
        assert_eq!(add.contest(on_wire), None);

        // The second maker is written in another module, which configures
        // the export there.
        let sites = through_set("crossModule");
        let add = add_through(&sites).expect("crossModule");
        assert_eq!(add.makers().len(), 2);
        let defaults = |on: &str, used: &MemberUse| match (on, used.member.as_deref()) {
            ("export", Some("defaults")) => MemberWire::ChangesName,
            _ => MemberWire::OnWire,
        };
        assert!(add.contest(defaults).is_some(), "{add:#?}");
        assert_eq!(add.contest(on_wire), None);
    }

    /// What is not one of a few makers' instances on every path stays read as
    /// nothing (carrick#1689): a `let` left unset on a path, set in a loop, in
    /// a closure, by `||=`, by `++`, in a `try`, by a destructuring, returned
    /// before it is set, from a loop or from a closure, or a `var` declared
    /// twice; a path that returns `null`, or calls a function that is no
    /// factory; five makers, directly or through two factories; and makers of
    /// two exports. A hand-off of any binding of the set contests it: the
    /// second binding of one maker (before, only the first binding's uses
    /// were read), one maker reached through two factories, and a second
    /// maker's binding.
    #[test]
    fn what_is_not_one_of_a_few_makers_on_every_path_is_read_as_nothing() {
        for factory in [
            "unsetPath",
            "inLoop",
            "inClosure",
            "compound",
            "updated",
            "nonInstance",
            "thirdPath",
            "ownMixed",
            "tooMany",
            "nestedTooMany",
            "returnedEarly",
            "returnInLoop",
            "nestedReturn",
            "inTry",
            "destructured",
            "twiceVar",
            "twoExports",
        ] {
            let sites = through_set(factory);
            assert!(add_through(&sites).is_none(), "{factory}: {sites:#?}");
        }
        for factory in ["twoBindings", "handedLet", "eitherA", "handedSecond"] {
            let sites = through_set(factory);
            let add = add_through(&sites).expect(factory);
            assert_eq!(add.contest(on_wire), Some(Contest::Used), "{factory}");
        }
    }

    /// Where a site's name is read, as a stand-in for the verified claims
    /// carrick#1662 reads: an argument of the call, or one of the maker's.
    enum NameAt {
        Arg(usize),
        MakerArg(usize),
    }

    /// The receiver ids whose maker verified, and the op elements that
    /// verified, each with its op and where its name is read.
    struct Verified<'a> {
        makers: &'a [&'a str],
        ops: Vec<(Selector<'a>, &'a str, NameAt)>,
    }

    /// What a site states under `verified`, folded over its receivers
    /// ([`LibrarySite::fold`]): through each, its maker verified, and every
    /// op element that applies to it read the same op and name.
    fn fact(site: &LibrarySite, verified: &Verified<'_>) -> Option<(String, String)> {
        site.fold(|through| {
            if let Some(maker) = through
                && !verified.makers.contains(&maker.receiver_id().as_str())
            {
                return None;
            }
            let mut read: Option<(String, String)> = None;
            for (selector, op, at) in &verified.ops {
                if !site.selected_by(through, selector) {
                    continue;
                }
                let name = match at {
                    NameAt::Arg(index) => site.literal(*index, None),
                    NameAt::MakerArg(index) => through
                        .and_then(|maker| maker.args.get(*index))
                        .and_then(|arg| arg.text.as_deref()),
                }?;
                let this = (op.to_string(), name.to_string());
                match &read {
                    Some(seen) if *seen != this => return None,
                    _ => read = Some(this),
                }
            }
            read
        })
    }

    /// A site through a set of makers states a fact only when every maker
    /// verified, an op element verified on each, and each reads the same op
    /// and name (carrick#1689, contract amendment 3): the op claimed on one
    /// maker only, a maker left unverified, and a name each maker's slot
    /// reads differently all leave it a candidate. A one-maker site and an
    /// export site fold as they read.
    #[test]
    fn a_set_of_makers_states_a_fact_only_when_every_maker_reads_the_same() {
        // `of` names one receiver; without it the op is on every instance.
        let on = |of: Option<&'static str>, member: &'static str| Selector {
            on: of.is_none().then_some(On::Instance),
            of,
            path: &[],
            member: Some(member),
        };
        let both = ["instance:new", "instance:new:Cluster"];

        let sites = through_set("createClient");
        let add = add_through(&sites).expect("createClient");
        let claimed = Verified {
            makers: &both,
            ops: vec![(on(None, "add"), "add", NameAt::Arg(0))],
        };
        assert_eq!(
            fact(add, &claimed),
            Some(("add".to_string(), "tick".to_string()))
        );
        let one_maker = Verified {
            makers: &both,
            ops: vec![(
                on(Some("instance:new:Cluster"), "add"),
                "add",
                NameAt::Arg(0),
            )],
        };
        assert_eq!(fact(add, &one_maker), None, "the op claimed on one maker");
        let unverified = Verified {
            makers: &["instance:new"],
            ops: vec![(on(None, "add"), "add", NameAt::Arg(0))],
        };
        assert_eq!(fact(add, &unverified), None, "a maker unverified");

        let sites = through_set("byArgs");
        let add = add_through(&sites).expect("byArgs");
        let from_maker = Verified {
            makers: &both,
            ops: vec![(on(None, "add"), "add", NameAt::MakerArg(0))],
        };
        assert_eq!(fact(add, &from_maker), None, "each maker names another");
        assert_eq!(
            fact(add, &claimed),
            Some(("add".to_string(), "tick".to_string())),
            "a name the call writes reads the same through each"
        );

        let sites = through_set("earlyThrow");
        let add = add_through(&sites).expect("earlyThrow");
        assert_eq!(
            fact(add, &from_maker),
            Some(("add".to_string(), "t".to_string()))
        );

        let sites = sites_of(&[(
            "src/direct.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             export async function go() { await Queue.add(\"direct\", {}); }\n",
        )]);
        let direct = site(&sites, "src/direct.ts", 2, Some("add"));
        let on_export = Verified {
            makers: &[],
            ops: vec![(
                Selector {
                    on: Some(On::Export),
                    of: None,
                    path: &[],
                    member: Some("add"),
                },
                "add",
                NameAt::Arg(0),
            )],
        };
        assert_eq!(
            fact(direct, &on_export),
            Some(("add".to_string(), "direct".to_string()))
        );
    }

    /// A name a function takes as a parameter and hands its library call is
    /// filled where the function is called (carrick#1562): a topic passed to
    /// a publish wrapper, an event passed to an emit wrapper, a name built
    /// into a template, and a parameter passed on through another wrapper.
    /// The library call itself states nothing for the hole, a caller states
    /// only what it fills, and a caller that passes anything but text, or
    /// spreads its arguments, fills nothing.
    #[test]
    fn a_name_passed_as_a_parameter_is_read_where_the_caller_writes_it() {
        let sites = sites_of(&[
            (
                "src/bus.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 import { io } from \"@fixture/socket\";\n\
                 export async function publish(topic: string, data: unknown) {\n\
                 \x20 await bus.publish(topic, data);\n\
                 }\n\
                 export async function publishOrder(kind: string, data: unknown) {\n\
                 \x20 await publish(`orders.${kind}`, data);\n\
                 }\n\
                 export async function audit(data: string) {\n\
                 \x20 await bus.publish(\"audit\", data);\n\
                 }\n\
                 export function emitTo(event: string, payload: unknown) {\n\
                 \x20 io.emit(event, payload);\n\
                 }\n\
                 export function enqueue(job: { name: string }) {\n\
                 \x20 bus.add(job);\n\
                 }\n",
            ),
            (
                "src/use.ts",
                "import { publish, publishOrder, audit, emitTo, enqueue } from \"./bus\";\n\
                 const TOPIC = \"users.created\";\n\
                 export async function a(x: unknown, dynamic: string) {\n\
                 \x20 await publish(\"users.deleted\", x);\n\
                 \x20 await publish(TOPIC, x);\n\
                 \x20 await publishOrder(\"shipped\", x);\n\
                 \x20 await audit(\"payload\");\n\
                 \x20 await publish(dynamic, x);\n\
                 \x20 const args: [string] = [\"spread\"];\n\
                 \x20 await publish(...args, \"late\");\n\
                 \x20 emitTo(\"joined\", x);\n\
                 \x20 enqueue({ name: \"welcome\" });\n\
                 }\n",
            ),
        ]);
        let wrapper = site(&sites, "src/bus.ts", 4, Some("publish"));
        assert_eq!(wrapper.literal(0, None), None, "a hole states nothing");
        assert!(wrapper.origin.is_none());

        let filled = |line: u32| {
            sites
                .iter()
                .find(|site| site.file.ends_with("src/use.ts") && site.line == line)
                .unwrap_or_else(|| panic!("a site at use.ts:{line}: {sites:#?}"))
        };
        for (line, name, origin_line) in [
            (4, "users.deleted", 4),
            (5, "users.created", 4),
            (6, "orders.shipped", 4),
            (11, "joined", 13),
        ] {
            let at = filled(line);
            assert_eq!(at.literal(0, None), Some(name), "use.ts:{line}");
            let origin = at.origin.as_ref().expect("filled by a caller");
            assert!(origin.file.ends_with("src/bus.ts"), "use.ts:{line}");
            assert_eq!(origin.line, origin_line, "use.ts:{line}");
            assert_eq!(at.receiver, SiteReceiver::Export, "use.ts:{line}");
        }
        assert_eq!(filled(11).specifier, "@fixture/socket");
        assert_eq!(filled(11).member.as_deref(), Some("emit"));
        // An argument that is one parameter is what the caller passes,
        // object and all.
        assert_eq!(filled(12).literal(0, Some("name")), Some("welcome"));

        let payload = filled(7);
        assert_eq!(
            payload.literal(0, None),
            None,
            "the name the library call states is not stated again"
        );
        assert_eq!(payload.literal(1, None), Some("payload"));

        for line in [8, 10] {
            assert!(
                sites
                    .iter()
                    .all(|site| !(site.file.ends_with("src/use.ts") && site.line == line)),
                "use.ts:{line} fills nothing: {sites:#?}"
            );
        }
        assert!(
            sites
                .iter()
                .all(|site| !(site.file.ends_with("src/bus.ts") && site.line == 7)),
            "a parameter passed on fills nothing where it is passed: {sites:#?}"
        );
    }

    /// The constants module the name tests read (carrick#1562).
    const TOPICS_MODULE: &str = "export const TOPICS = { orders: { created: \"orders.created\" }, users: \"users\" } as const;\n\
         export const PREFIX = \"svc\";\n\
         export const topicFor = (kind: string) => `${PREFIX}.${kind}`;\n\
         export function auditTopic(name: string) { return `audit.${name}`; }\n\
         export const ROUTES = { byId: (id: string) => `jobs.${id}` };\n\
         export const MUTABLE = { a: \"mutable.a\" };\n\
         export function twoSteps(name: string) { const topic = `two.${name}`; return topic; }\n";

    /// A name read through a binding the service declares (carrick#1562):
    /// an entry of a constant object, an imported constant, and what a
    /// builder returns, a builder in an object included, read wherever the
    /// binding is declared. A builder called with a parameter leaves a hole
    /// its callers fill. An object some module changes, or whose inner
    /// object is aliased, and a function that does more than return text
    /// are read as nothing.
    #[test]
    fn a_name_read_through_a_map_a_constant_or_a_builder_is_its_text() {
        let sites = sites_of(&[
            ("src/topics.ts", TOPICS_MODULE),
            (
                "src/other.ts",
                "import { MUTABLE } from \"./topics\";\n\
                 MUTABLE.a = \"changed\";\n",
            ),
            (
                "src/use.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 import { TOPICS, PREFIX, topicFor, auditTopic, ROUTES, MUTABLE, twoSteps } from \"./topics\";\n\
                 const LOCAL = { shipped: \"orders.shipped\" };\n\
                 const NESTED = { inner: { x: \"nested.x\" } };\n\
                 const alias = NESTED.inner;\n\
                 export function a(kind: string) {\n\
                 \x20 bus.publish(TOPICS.orders.created, {});\n\
                 \x20 bus.publish(PREFIX, {});\n\
                 \x20 bus.publish(topicFor(\"x\"), {});\n\
                 \x20 bus.publish(auditTopic(\"y\"), {});\n\
                 \x20 bus.publish(ROUTES.byId(\"7\"), {});\n\
                 \x20 bus.publish(LOCAL.shipped, {});\n\
                 \x20 bus.publish(MUTABLE.a, {});\n\
                 \x20 bus.publish(NESTED.inner.x, {});\n\
                 \x20 bus.publish(topicFor(kind), {});\n\
                 \x20 bus.publish(twoSteps(\"z\"), {});\n\
                 \x20 bus.publish(TOPICS.users, alias);\n\
                 }\n",
            ),
            (
                "src/caller.ts",
                "import { a } from \"./use\";\n\
                 export function go() { a(\"filled\"); }\n",
            ),
        ]);
        let name = |line: u32| {
            site(&sites, "src/use.ts", line, Some("publish"))
                .literal(0, None)
                .map(str::to_string)
        };
        assert_eq!(name(7).as_deref(), Some("orders.created"), "a nested entry");
        assert_eq!(name(8).as_deref(), Some("svc"), "an imported constant");
        assert_eq!(name(9).as_deref(), Some("svc.x"), "an imported builder");
        assert_eq!(name(10).as_deref(), Some("audit.y"), "a builder function");
        assert_eq!(
            name(11).as_deref(),
            Some("jobs.7"),
            "a builder in an object"
        );
        assert_eq!(name(12).as_deref(), Some("orders.shipped"), "an own object");
        assert_eq!(name(13), None, "an object another module writes through");
        assert_eq!(name(14), None, "an object whose inner object is aliased");
        assert_eq!(name(15), None, "a builder handed a parameter");
        assert_eq!(name(16), None, "a function that does more than return");
        assert_eq!(name(17).as_deref(), Some("users"));
        let filled = site(&sites, "src/caller.ts", 2, Some("publish"));
        assert_eq!(filled.literal(0, None), Some("svc.filled"));
    }

    /// A constant object used any way but to read an entry holds nothing a
    /// name reads (carrick#1562): handed to a call, spread (a copy shares its
    /// inner objects), returned, read by a key the source does not state, or
    /// read through an optional chain. An inner object read in a test is an
    /// inner object read, as it was before carrick#1690.
    #[test]
    fn an_object_used_any_way_but_read_holds_no_name() {
        let sites = sites_of(&[
            (
                "src/topics.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 import { register } from \"./registry\";\n\
                 const HANDED = { a: \"handed.a\" };\n\
                 const SPREAD = { a: \"spread.a\" };\n\
                 const RETURNED = { a: \"returned.a\" };\n\
                 const COMPUTED = { a: \"computed.a\" };\n\
                 const OPTIONAL = { inner: { a: \"optional.a\" } };\n\
                 const KEPT = { a: \"kept.a\" };\n\
                 const TESTED_INNER = { inner: { a: \"tested.a\" } };\n\
                 const TESTED_TEXT = { a: \"tested.text\" };\n\
                 register(HANDED);\n\
                 export const copy = { ...SPREAD };\n\
                 export function give() { return RETURNED; }\n\
                 export function pick(key: \"a\") { return COMPUTED[key]; }\n\
                 export function maybe() { return OPTIONAL?.inner; }\n\
                 export function check() { if (TESTED_INNER.inner || TESTED_TEXT.a) { return 1; } return 0; }\n\
                 export function send() {\n\
                 \x20 bus.publish(HANDED.a, {});\n\
                 \x20 bus.publish(SPREAD.a, {});\n\
                 \x20 bus.publish(RETURNED.a, {});\n\
                 \x20 bus.publish(COMPUTED.a, {});\n\
                 \x20 bus.publish(OPTIONAL.inner.a, {});\n\
                 \x20 bus.publish(KEPT.a, {});\n\
                 \x20 bus.publish(TESTED_INNER.inner.a, {});\n\
                 \x20 bus.publish(TESTED_TEXT.a, {});\n\
                 }\n",
            ),
            (
                "src/registry.ts",
                "export function register(value: unknown) {}\n",
            ),
        ]);
        let name = |line: u32| {
            site(&sites, "src/topics.ts", line, Some("publish"))
                .literal(0, None)
                .map(str::to_string)
        };
        assert_eq!(name(18), None, "handed to a call");
        assert_eq!(name(19), None, "spread");
        assert_eq!(name(20), None, "returned");
        assert_eq!(name(21), None, "read by a computed key");
        assert_eq!(name(22), None, "read through an optional chain");
        assert_eq!(name(23).as_deref(), Some("kept.a"), "the control");
        assert_eq!(name(24), None, "an inner object read in a test");
        assert_eq!(
            name(25).as_deref(),
            Some("tested.text"),
            "a text entry read in a test"
        );
    }

    /// A constant object is read only where every module that reaches it
    /// leaves it as it is (carrick#1562): a namespace import of its module
    /// may change it unseen, though not its text constants. A module the
    /// scan cannot follow turns every imported name off, as it does every
    /// imported instance.
    #[test]
    fn a_constant_object_any_module_may_change_is_read_as_nothing() {
        let user = "import { bus } from \"@fixture/bus\";\n\
                    import { TOPICS, PREFIX } from \"./topics\";\n\
                    export function a() {\n\
                    \x20 bus.publish(TOPICS.users, {});\n\
                    \x20 bus.publish(PREFIX, {});\n\
                    }\n";
        let name = |sites: &[LibrarySite], line: u32| {
            site(sites, "src/use.ts", line, Some("publish"))
                .literal(0, None)
                .map(str::to_string)
        };
        let control = sites_of(&[("src/topics.ts", TOPICS_MODULE), ("src/use.ts", user)]);
        assert_eq!(name(&control, 4).as_deref(), Some("users"));

        let namespaced = sites_of(&[
            ("src/topics.ts", TOPICS_MODULE),
            ("src/use.ts", user),
            (
                "src/all.ts",
                "import * as topics from \"./topics\";\n\
                 export const every = topics;\n",
            ),
        ]);
        assert_eq!(name(&namespaced, 4), None, "a namespace import");
        assert_eq!(name(&namespaced, 5).as_deref(), Some("svc"));

        let unfollowable = sites_of(&[
            ("src/topics.ts", TOPICS_MODULE),
            ("src/use.ts", user),
            (
                "src/setup.ts",
                "import { setup } from \"~/nowhere\";\n\
                 setup();\n",
            ),
        ]);
        assert_eq!(
            name(&unfollowable, 4),
            None,
            "a module the scan cannot follow"
        );
        assert_eq!(name(&unfollowable, 5), None, "an imported constant too");
    }

    /// A caller-filled site carries the library call's contest: a receiver
    /// taken away is taken away at every caller (carrick#1562). A hole in a
    /// function written inside the wrapper is that function's parameter,
    /// which no caller of the wrapper passes.
    #[test]
    fn a_filled_site_is_contested_as_its_library_call_is() {
        let sites = sites_of(&[
            (
                "src/bus.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 import { register } from \"./registry\";\n\
                 register(bus);\n\
                 export async function publish(topic: string) {\n\
                 \x20 await bus.publish(topic, {});\n\
                 }\n\
                 export function each(prefix: string) {\n\
                 \x20 [prefix].forEach((topic) => bus.publish(topic, {}));\n\
                 }\n",
            ),
            (
                "src/registry.ts",
                "export function register(value: unknown) {}\n",
            ),
            (
                "src/use.ts",
                "import { publish, each } from \"./bus\";\n\
                 export async function a() {\n\
                 \x20 await publish(\"orders\");\n\
                 \x20 each(\"one\");\n\
                 }\n",
            ),
        ]);
        let at = site(&sites, "src/use.ts", 3, Some("publish"));
        assert_eq!(at.literal(0, None), Some("orders"));
        assert_eq!(at.contest(on_wire), Some(Contest::Used));
        assert!(
            sites
                .iter()
                .all(|site| !(site.file.ends_with("src/use.ts") && site.line == 4)),
            "{sites:#?}"
        );
    }
}
