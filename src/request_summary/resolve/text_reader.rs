//! The resolver's text reader (carrick#3504 S2): the one text a name's
//! expression always is, where the code states every part of it and the
//! compiler names each declaration it reads (carrick#3479, moved here from
//! the name fold).
//!
//! The compiler types many constants wider than their value: a `let` is
//! `string`, an enum member is its enum, an object literal's entry is
//! `string`, a built-in's result is `string`. Each still has one value the
//! language fixes. The terms come from [`super::text_of`]; the declarations
//! are the binding table's ([`super::ModuleBinding`]), so a `let` the module
//! writes again, a `var`, and an object whose entries the module may change
//! are holes, by the one write census.
//!
//! A reference to a file the run does not hold (a declaration outside the
//! service and its siblings) is [`guards::HoleReason::OutsideService`]: there
//! is no disk fallback.

use super::flow::WayFrom;
use super::*;

/// One text a name may be, read by [`Resolver::text_values`] (carrick#3504
/// S2). Never collapsed: the consumer applies its own policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) enum TextValue {
    /// The name always is `text`, read through the declarations in
    /// `read_from` (repo files, as a record names a declaration).
    Text {
        text: String,
        read_from: Vec<String>,
        /// One literal of a union the compiler types a choice's branch as:
        /// the type states the choice, not the code (record rule 2.3.6), so
        /// a set holding it stays asked.
        from_type: bool,
        /// The calls the text was read through, a parameter's caller at a
        /// time, innermost first ([`Resolver::texts_through_callers`],
        /// carrick#3504 B10-1b): the last is the call where it settles.
        /// Empty for every other entry point.
        callers: Vec<CallAt>,
        /// Each loop the text was read through and the element it took
        /// (where the loop starts, the element's index; carrick#2093): two
        /// parts join only where they took the same element of every loop
        /// both read. Empty for a text read through no loop.
        elements: Vec<(u32, usize)>,
    },
    /// The name is none the reader states, for `reason`, read through
    /// `callers` as a text is.
    Hole {
        reason: guards::HoleReason,
        callers: Vec<CallAt>,
    },
    /// The name as parts ([`Resolver::pieces_through_callers`], carrick#3504
    /// B10-2a2): produced only in that mode.
    Pieced(PiecedText),
}

/// A name read through callers as parts (carrick#3504 B10-2a2, Q2): each
/// part a literal or a hole, with where it was read and the parameters it
/// was passed through, so a consumer can spell each part as the request
/// summaries read it ([`FileIr::arg_pieces`]) and place it at a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) struct PiecedText {
    pub(in crate::request_summary) parts: Vec<TextPart>,
    /// The calls the value was read through, innermost first, as
    /// [`TextValue::Text`] carries them.
    pub(in crate::request_summary) callers: Vec<CallAt>,
    pub(in crate::request_summary) from_type: bool,
}

/// One part of a [`PiecedText`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) struct TextPart {
    pub(in crate::request_summary) value: PartValue,
    /// The term the part was read at, in the numbering `arg_texts` uses;
    /// `None` for text written in place (a template's quasi, a literal).
    pub(in crate::request_summary) at: Option<SpanAt>,
    /// Each parameter the part was passed through, innermost first.
    pub(in crate::request_summary) through: Vec<Hop>,
}

/// What a part states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) enum PartValue {
    Lit {
        text: String,
        read_from: Vec<String>,
    },
    Hole(guards::HoleReason),
}

/// A term's span in a file of the run, as `inputs` keys the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) struct SpanAt {
    pub(in crate::request_summary) file: PathBuf,
    pub(in crate::request_summary) lo: u32,
    pub(in crate::request_summary) hi: u32,
}

/// One parameter a part was passed through: the call that fills it, and the
/// parameter read inside the function the call fills. A keyed read the code
/// does not write (a slot's key, an entry read off an argument handed to the
/// parameter) is its object's span, with the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) struct Hop {
    pub(in crate::request_summary) call: CallAt,
    pub(in crate::request_summary) read: SpanAt,
    pub(in crate::request_summary) keys: Vec<String>,
}

impl PiecedText {
    /// The reason of the first part that is a hole.
    pub(in crate::request_summary) fn first_hole(&self) -> Option<guards::HoleReason> {
        self.parts.iter().find_map(|part| match part.value {
            PartValue::Hole(reason) => Some(reason),
            PartValue::Lit { .. } => None,
        })
    }

    /// One part, read through no parameter.
    fn one(value: PartValue, at: Option<SpanAt>, callers: Vec<CallAt>) -> Self {
        PiecedText {
            parts: vec![TextPart {
                value,
                at,
                through: Vec::new(),
            }],
            callers,
            from_type: false,
        }
    }
}

/// The span of `term` in `file`, where the lowering keeps one.
fn span_of(file: &Path, term: &ValueAt) -> Option<SpanAt> {
    let (lo, hi) = match term {
        ValueAt::Read { lo, hi, .. }
        | ValueAt::Field { lo, hi, .. }
        | ValueAt::Returned { lo, hi, .. }
        | ValueAt::New { lo, hi, .. }
        | ValueAt::Constructed { lo, hi, .. }
        | ValueAt::KeyOf { lo, hi, .. }
        | ValueAt::Member { lo, hi, .. }
        | ValueAt::BuiltIn { lo, hi, .. }
        | ValueAt::Alt { lo, hi, .. }
        | ValueAt::Unstated { lo, hi } => (*lo, *hi),
        _ => return None,
    };
    // `handed`'s wrap is written nowhere.
    ((lo, hi) != (0, 0)).then(|| SpanAt {
        file: file.to_path_buf(),
        lo,
        hi,
    })
}

/// How the open text query reads a parameter (carrick#3504 B10-2a2, Q1):
/// not at all (every entry point but the two below), as the whole texts its
/// callers pass ([`Resolver::texts_through_callers`]), or as parts
/// ([`Resolver::pieces_through_callers`]). Both callers modes share the one
/// caller walk ([`Resolver::param_texts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::request_summary) enum CallersRead {
    Off,
    Whole,
    Pieces,
}

/// One call a parameter was read at (carrick#3504 B10-1b): the caller's file
/// as `inputs` keys it, the call's span in discovery numbering, and the
/// definition (its file and key) whose parameter the call fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) struct CallAt {
    pub(in crate::request_summary) file: PathBuf,
    pub(in crate::request_summary) lo: u32,
    pub(in crate::request_summary) hi: u32,
    pub(in crate::request_summary) fills: (PathBuf, String),
}

/// What the resolver reads of an argument of a maker call
/// ([`Resolver::keyed_text_set`], `NameFold::arg_texts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) enum ArgTexts {
    /// Every text the argument, or its entry, may be.
    Values(Vec<TextValue>),
    /// An options object the resolver cannot read: a spread that may write
    /// the key with none written, an exported object, one handed to more than
    /// one call. It keeps today's host-free reading and is counted (carrick
    /// #3504 S5a, R3), with why it is unread (carrick#3688 U2).
    Unread(Stop),
}

impl TextValue {
    /// A text the code states, read through `read_from`.
    fn stated(text: String, read_from: Vec<String>) -> Self {
        TextValue::Text {
            text,
            read_from,
            from_type: false,
            callers: Vec::new(),
            elements: Vec::new(),
        }
    }

    /// The loop elements this value was read through ([`TextValue::Text`]'s
    /// `elements`); none for a hole or parts.
    fn elements(&self) -> &[(u32, usize)] {
        match self {
            TextValue::Text { elements, .. } => elements,
            TextValue::Hole { .. } | TextValue::Pieced(_) => &[],
        }
    }

    /// A hole of `reason`, read through no caller.
    pub(in crate::request_summary) fn hole(reason: guards::HoleReason) -> Self {
        TextValue::Hole {
            reason,
            callers: Vec::new(),
        }
    }

    /// The calls this value was read through, innermost first; empty
    /// outside [`Resolver::texts_through_callers`].
    pub(in crate::request_summary) fn callers(&self) -> &[CallAt] {
        match self {
            TextValue::Text { callers, .. }
            | TextValue::Hole { callers, .. }
            | TextValue::Pieced(PiecedText { callers, .. }) => callers,
        }
    }

    /// This value, read one caller further out: `at` goes first on its
    /// chain. Read as parts, each part was passed through the parameter
    /// read `read` (with its keys), which goes first on its hops.
    fn read_at(mut self, at: &CallAt, read: Option<&(SpanAt, Vec<String>)>) -> Self {
        match &mut self {
            TextValue::Text { callers, .. } | TextValue::Hole { callers, .. } => {
                callers.insert(0, at.clone());
            }
            TextValue::Pieced(pieced) => {
                pieced.callers.insert(0, at.clone());
                if let Some((read, keys)) = read {
                    for part in &mut pieced.parts {
                        part.through.insert(
                            0,
                            Hop {
                                call: at.clone(),
                                read: read.clone(),
                                keys: keys.clone(),
                            },
                        );
                    }
                }
            }
        }
        self
    }

    /// This value as parts: a whole text or a hole is one part, read at
    /// `at`.
    fn into_pieced(self, at: Option<SpanAt>) -> PiecedText {
        match self {
            TextValue::Pieced(pieced) => pieced,
            TextValue::Hole { reason, callers } => {
                PiecedText::one(PartValue::Hole(reason), at, callers)
            }
            TextValue::Text {
                text,
                read_from,
                from_type,
                callers,
                ..
            } => PiecedText {
                from_type,
                ..PiecedText::one(PartValue::Lit { text, read_from }, at, callers)
            },
        }
    }
}

/// Whether two chains agree on every definition both fill (carrick#3504
/// B10-1b, P5): parts read at different calls of one function never join.
fn chains_agree(head: &[CallAt], tail: &[CallAt]) -> bool {
    head.iter()
        .all(|a| tail.iter().filter(|b| b.fills == a.fills).all(|b| b == a))
}

/// Whether every value of one caller chain is a text read through a loop
/// over a stated list (carrick#2093), each from its own element: then the
/// texts are one name per element, never a choice between them. A text read
/// through no loop, or two texts from one element (a choice written inside
/// the loop), keeps the several names a choice (carrick#4245).
///
/// The one answer to "is this set one name per element of the loops it was
/// read through?": the library reader (`SiteNames`) and the site path
/// (`site_record`) both ask it here.
pub(in crate::request_summary) fn per_element(held: &[&TextValue]) -> bool {
    let mut seen: Vec<&[(u32, usize)]> = Vec::new();
    for value in held {
        let TextValue::Text {
            elements,
            from_type: false,
            ..
        } = value
        else {
            return false;
        };
        if elements.is_empty() || seen.contains(&elements.as_slice()) {
            return false;
        }
        seen.push(elements);
    }
    true
}

/// Whether two values took the same element of every loop both were read
/// through (carrick#2093): `${a}/${b}` over one tuple list joins each
/// tuple's own names, never one tuple's `a` with another's `b`.
fn elements_agree(head: &[(u32, usize)], tail: &[(u32, usize)]) -> bool {
    head.iter().all(|(loop_at, index)| {
        tail.iter()
            .filter(|(other, _)| other == loop_at)
            .all(|(_, other)| other == index)
    })
}

/// Whether two values may be joined: their caller chains agree
/// ([`chains_agree`]) and so do their loop elements ([`elements_agree`]).
fn values_agree(head: &TextValue, tail: &TextValue) -> bool {
    chains_agree(head.callers(), tail.callers()) && elements_agree(head.elements(), tail.elements())
}

/// The union of two chains that agree: `head`'s calls, then `tail`'s it
/// does not hold.
fn chains_joined(head: &[CallAt], tail: &[CallAt]) -> Vec<CallAt> {
    let mut out = head.to_vec();
    for at in tail {
        if !out.contains(at) {
            out.push(at.clone());
        }
    }
    out
}

/// Counts one more value on `chain` (carrick#3504 B10-2a2, Q6: the cap
/// counts within one caller chain, never across chains).
fn count_chain(counts: &mut Vec<(Vec<CallAt>, usize)>, chain: &[CallAt]) {
    match counts.iter_mut().find(|(held, _)| held.as_slice() == chain) {
        Some((_, count)) => *count += 1,
        None => counts.push((chain.to_vec(), 1)),
    }
}

/// Caps the values read through callers within each chain (carrick#3504
/// B10-2a2, Q6): a chain with more than [`MAX_VALUES`] is one cap hole on
/// that chain. Values across chains are never capped.
fn cap_each_chain(out: &mut Vec<TextValue>) {
    let mut counts: Vec<(Vec<CallAt>, usize)> = Vec::new();
    for value in out.iter() {
        count_chain(&mut counts, value.callers());
    }
    for (chain, count) in counts {
        if count > MAX_VALUES {
            out.retain(|value| value.callers() != chain.as_slice());
            out.push(TextValue::Hole {
                reason: guards::HoleReason::FanOutCap,
                callers: chain,
            });
        }
    }
}

/// What `term`, handed to a parameter, holds down `keys`: an object literal's
/// last entry with each key (`None` where it writes none: the key is
/// omitted), else a read of the key off the term, as
/// [`Resolver::keyed_text_set`] reads one. Also how a slot's key is read off
/// the labelled call's argument (carrick#3504 B10-2a2).
pub(in crate::request_summary) fn handed(term: &ValueAt, keys: &[String]) -> Option<ValueAt> {
    let mut term = term.clone();
    for key in keys {
        term = match term {
            ValueAt::Entries(entries)
                if entries
                    .iter()
                    .all(|(name, _)| matches!(name, ValueAt::Lit(_))) =>
            {
                entries
                    .into_iter()
                    .rev()
                    .find(|(name, _)| matches!(name, ValueAt::Lit(name) if name == key))?
                    .1
            }
            other => ValueAt::Member {
                lo: 0,
                hi: 0,
                object: Box::new(other),
                key: Box::new(ValueAt::Lit(key.clone())),
            },
        };
    }
    Some(term)
}

/// Sets the open query's [`CallersRead`] until it drops, then puts back what
/// was there.
struct CallersMode<'r> {
    mode: &'r Cell<CallersRead>,
    before: CallersRead,
}

impl<'r> CallersMode<'r> {
    fn set(mode: &'r Cell<CallersRead>, to: CallersRead) -> Self {
        CallersMode {
            mode,
            before: mode.replace(to),
        }
    }
}

impl Drop for CallersMode<'_> {
    fn drop(&mut self) {
        self.mode.set(self.before);
    }
}

impl<'a> Resolver<'a> {
    /// The one text `value`, written in `file`, always is, where the code
    /// states every part of it and the compiler names each declaration it
    /// reads (carrick#3479, moved onto the binding table):
    ///
    /// - a name the compiler types as one string literal is that literal;
    /// - a `const`, or a `let` its module never assigns again (an importer
    ///   cannot), is its initialiser, read at the declaration the row names;
    /// - an enum member is its initialiser;
    /// - an entry of a `const` object literal is its value, where the module
    ///   only ever reads the object's entries, and a computed key is the
    ///   text its expression states;
    /// - `join`, `concat`, `replace`, `replaceAll` and `String.raw` over
    ///   such parts, each only where the compiler's row says the call is the
    ///   language library's, and templates and `+` over them.
    ///
    /// Anything else (a parameter, a call of the service's own, a value read
    /// at runtime, a replacement that names a `$` pattern, a `var`) is a
    /// hole of its reason.
    pub(super) fn text_values(&self, file: &Path, value: &ValueAt) -> Vec<TextValue> {
        let _query = self.enter();
        if self.step() {
            return vec![TextValue::hole(guards::HoleReason::FanOutCap)];
        }
        let out = self.text_in(file, value);
        // Read through callers, the values read before the cap stand, and
        // each read after it is a cap hole on its own chain (carrick#3504
        // B10-2a2, Q6).
        if self.open.get().0 == 1
            && self.capped.get()
            && self.callers_mode.get() == CallersRead::Off
        {
            return vec![TextValue::hole(guards::HoleReason::FanOutCap)];
        }
        out
    }

    /// Every text `value`, written in `file` (as `inputs` keys it), may be,
    /// as [`Self::text_values`] reads it, except that a parameter reads as
    /// the texts its callers pass (carrick#3504 B10-1b). Each value carries
    /// the calls it was read through, innermost first, and is never
    /// collapsed: one per caller chain. A parameter of a definition nothing
    /// calls is a [`guards::HoleReason::NameNeverBound`] hole (beside its
    /// default, if any), one of a function written inside a body a
    /// [`guards::HoleReason::CallbackParameter`] hole. A caller that passes
    /// the parameter on unchanged inside a cycle adds nothing; a changed
    /// value in a cycle is a [`guards::HoleReason::Recursion`] hole.
    ///
    /// The one entry that reads through callers: every other entry point
    /// answers a parameter as before, and [`Self::one_text`] refuses a text
    /// a caller decides.
    pub(in crate::request_summary) fn texts_through_callers(
        &self,
        file: &Path,
        value: &ValueAt,
    ) -> Vec<TextValue> {
        let _mode = CallersMode::set(&self.callers_mode, CallersRead::Whole);
        self.text_values(file, value)
    }

    /// Every value `value`, written in `file` (as `inputs` keys it), may be
    /// through its callers, as [`Self::texts_through_callers`] reads it, but
    /// as parts (carrick#3504 B10-2a2, Q1-Q3): one per caller chain, never
    /// collapsed. A term splits only on its own structure (a template's or a
    /// `+` chain's parts), on a local the same function body declares and
    /// never writes again, and on parameter reads, which read each caller's
    /// term split the same way, with the parameter read as a [`Hop`]. Any
    /// other term (a choice, a builder call, a module constant, `this.x`, an
    /// environment read) is one part: its one text, else a hole at its span.
    /// A parameter is read before the one literal its type may be.
    pub(in crate::request_summary) fn pieces_through_callers(
        &self,
        file: &Path,
        value: &ValueAt,
    ) -> Vec<PiecedText> {
        let _mode = CallersMode::set(&self.callers_mode, CallersRead::Pieces);
        let at = span_of(file, value);
        self.text_values(file, value)
            .into_iter()
            .map(|value| value.into_pieced(at.clone()))
            .collect()
    }

    /// `value` as parts, read in pieces mode (Q3).
    fn pieces_in(&self, file: &Path, value: &ValueAt) -> Vec<TextValue> {
        match value {
            // An empty literal writes nothing.
            ValueAt::Lit(text) if text.is_empty() => vec![TextValue::Pieced(PiecedText {
                parts: Vec::new(),
                callers: Vec::new(),
                from_type: false,
            })],
            ValueAt::Lit(text) => vec![TextValue::Pieced(PiecedText::one(
                PartValue::Lit {
                    text: text.clone(),
                    read_from: Vec::new(),
                },
                None,
                Vec::new(),
            ))],
            ValueAt::Concat(parts) => self.pieces_joined(file, parts),
            ValueAt::Read {
                lo,
                hi,
                member: false,
                ..
            } => {
                let read = SpanAt {
                    file: file.to_path_buf(),
                    lo: *lo,
                    hi: *hi,
                };
                if let Ok(row) = self.row_at(file, *lo, *hi, Kind::Ident) {
                    if let Some((at, param)) = self.declared_param(row) {
                        return self.param_pieces(at, param, &[], read, Vec::new());
                    }
                    if let Some(split) = self.local_pieces(row, &read) {
                        return split;
                    }
                }
                self.one_part(file, value)
            }
            ValueAt::Member {
                lo,
                hi,
                object,
                key,
            } => {
                if let ValueAt::Read {
                    lo: object_lo,
                    hi: object_hi,
                    member: false,
                    ..
                } = &**object
                    && let Ok(row) = self.row_at(file, *object_lo, *object_hi, Kind::Ident)
                    && let Some((at, param)) = self.declared_param(row)
                {
                    let key = {
                        let _whole = CallersMode::set(&self.callers_mode, CallersRead::Whole);
                        self.one_text(file, key)
                    };
                    let key = match key {
                        Ok((key, _)) => key,
                        Err(reason) => {
                            return vec![TextValue::Pieced(PiecedText::one(
                                PartValue::Hole(reason),
                                span_of(file, value),
                                Vec::new(),
                            ))];
                        }
                    };
                    // A read the code does not write (`handed`'s wrap) is
                    // its object's, down the key.
                    let ((read_lo, read_hi), keys) = if (*lo, *hi) == (0, 0) {
                        ((*object_lo, *object_hi), vec![key.clone()])
                    } else {
                        ((*lo, *hi), Vec::new())
                    };
                    let read = SpanAt {
                        file: file.to_path_buf(),
                        lo: read_lo,
                        hi: read_hi,
                    };
                    return self.param_pieces(at, param, std::slice::from_ref(&key), read, keys);
                }
                self.one_part(file, value)
            }
            // A field its constructor sets from a parameter splits like a
            // parameter read: each construction's term, with the field read
            // as its hop (carrick#3678 S6-2).
            ValueAt::Field {
                lo,
                hi,
                class,
                field,
            } => {
                let declared = {
                    let _whole = CallersMode::set(&self.callers_mode, CallersRead::Whole);
                    self.member_declared(file, (*lo, *hi), &mut Vec::new())
                };
                let hop = (
                    SpanAt {
                        file: file.to_path_buf(),
                        lo: *lo,
                        hi: *hi,
                    },
                    Vec::new(),
                );
                match declared {
                    None => match self.field_texts(file, *class, field, Some(&hop)) {
                        Some(values) => values
                            .into_iter()
                            .map(|value| TextValue::Pieced(value.into_pieced(Some(hop.0.clone()))))
                            .collect(),
                        None => self.one_part(file, value),
                    },
                    Some(_) => self.one_part(file, value),
                }
            }
            _ => self.one_part(file, value),
        }
    }

    /// `value` as the one part it is: its one text where the whole reader
    /// reads exactly one with no caller deciding it, else a hole of why not,
    /// at its span.
    fn one_part(&self, file: &Path, value: &ValueAt) -> Vec<TextValue> {
        let read = {
            let _whole = CallersMode::set(&self.callers_mode, CallersRead::Whole);
            self.one_text(file, value)
        };
        let part = match read {
            Ok((text, read_from)) => PartValue::Lit { text, read_from },
            Err(reason) => PartValue::Hole(reason),
        };
        vec![TextValue::Pieced(PiecedText::one(
            part,
            span_of(file, value),
            Vec::new(),
        ))]
    }

    /// The parameter `param`, read at `read`, as parts: each caller's term
    /// split, with the read as its hop; a parameter no caller states is one
    /// hole at the read.
    fn param_pieces(
        &self,
        at: (&'a Path, u32, String),
        param: &ParamAt,
        more: &[String],
        read: SpanAt,
        keys: Vec<String>,
    ) -> Vec<TextValue> {
        let hop = (read, keys);
        self.param_texts(at, param, more, Some(&hop))
            .into_iter()
            .map(|value| TextValue::Pieced(value.into_pieced(Some(hop.0.clone()))))
            .collect()
    }

    /// A local `row` names, read at `read`, split as its initialiser
    /// (Q3): a `const`, or a `let` never written again, that a function
    /// body declares. `None` where it is no such local, and where the
    /// request summaries read it by other pieces than its initialiser's
    /// parts (a local of an opaque base and a parameter, or one holding a
    /// parameter's key, is read by its own name): there it is one part, so
    /// it is spelled as they read it.
    fn local_pieces(&self, row: &Row, read: &SpanAt) -> Option<Vec<TextValue>> {
        let (at, binding, _) = self.declared_entry(row)?;
        let once = match binding.kind {
            BindingKind::Const => !binding.unplaceable,
            BindingKind::Let => binding.writes.is_empty() && !binding.unplaceable,
            _ => false,
        };
        if !matches!(binding.declared, Declared::Body(_)) || !once {
            return None;
        }
        let text = binding.text.as_ref()?;
        let split = {
            let _reading = self.reading_text(&at)?;
            self.text_values(at.0, text)
        };
        let summaries = self
            .inputs_for(&read.file)
            .files
            .get(&read.file)
            .and_then(|ir| ir.arg_pieces.get(&(read.lo, read.hi)));
        if let Some(summaries) = summaries {
            for value in &split {
                let TextValue::Pieced(pieced) = value else {
                    return None;
                };
                if super::super::decided_sites::rendered(self, pieced, 0) != *summaries {
                    return None;
                }
            }
        }
        Some(split)
    }

    /// Every value `parts`, joined in order, may be, as parts: one per
    /// choice of a value of each part, joined only where their chains agree
    /// (as [`Self::joined`]). [`MAX_VALUES`] counts the joinings within one
    /// chain (Q6): a chain with more is one cap hole.
    fn pieces_joined(&self, file: &Path, parts: &[ValueAt]) -> Vec<TextValue> {
        let mut out = vec![PiecedText {
            parts: Vec::new(),
            callers: Vec::new(),
            from_type: false,
        }];
        for part in parts {
            if matches!(part, ValueAt::Lit(text) if text.is_empty()) {
                continue;
            }
            let at = span_of(file, part);
            let values: Vec<PiecedText> = self
                .text_values(file, part)
                .into_iter()
                .map(|value| value.into_pieced(at.clone()))
                .collect();
            let mut next: Vec<PiecedText> = Vec::new();
            let mut counts: Vec<(Vec<CallAt>, usize)> = Vec::new();
            for head in &out {
                for tail in &values {
                    if !chains_agree(&head.callers, &tail.callers) {
                        continue;
                    }
                    let callers = chains_joined(&head.callers, &tail.callers);
                    count_chain(&mut counts, &callers);
                    let mut joined_parts = head.parts.clone();
                    joined_parts.extend(tail.parts.iter().cloned());
                    let joined = PiecedText {
                        parts: joined_parts,
                        callers,
                        from_type: head.from_type || tail.from_type,
                    };
                    if !next.contains(&joined) {
                        next.push(joined);
                    }
                }
            }
            for (chain, count) in counts {
                if count > MAX_VALUES {
                    next.retain(|value| value.callers != chain);
                    next.push(PiecedText::one(
                        PartValue::Hole(guards::HoleReason::FanOutCap),
                        None,
                        chain,
                    ));
                }
            }
            out = next;
        }
        out.into_iter().map(TextValue::Pieced).collect()
    }

    fn text_in(&self, file: &Path, value: &ValueAt) -> Vec<TextValue> {
        use guards::HoleReason::*;
        if self.callers_mode.get() == CallersRead::Pieces {
            return self.pieces_in(file, value);
        }
        let hole = |reason| vec![TextValue::hole(reason)];
        match value {
            ValueAt::Lit(text) => vec![TextValue::stated(text.clone(), Vec::new())],
            ValueAt::Concat(parts) => self.joined(file, parts),
            ValueAt::Read {
                lo,
                hi,
                member: false,
                ..
            } => self.text_of_name(file, *lo, *hi),
            ValueAt::Alt {
                branches, nullish, ..
            } => {
                // `a ?? b` holds `b` only where `a` may be nullish: a left
                // the compiler types as one non-nullable text is the value
                // alone. A nullable left is both sides, and a left no
                // reader states stays a hole beside `b`, so no consumer
                // takes `b` alone (carrick#3835).
                let held = match branches.as_slice() {
                    [left, _] if *nullish && self.is_non_nullable_text(file, left) => {
                        &branches[..1]
                    }
                    all => all,
                };
                held.iter()
                    .flat_map(|branch| self.text_of_branch(file, branch))
                    .collect()
            }
            ValueAt::Member {
                lo,
                hi,
                object,
                key,
            } => self.text_of_member(file, (*lo, *hi), object, key),
            ValueAt::Field {
                lo,
                hi,
                class,
                field,
            } => self.text_of_field(file, (*lo, *hi), *class, field),
            ValueAt::BuiltIn {
                lo,
                hi,
                method,
                object,
                args,
            } => self.text_of_built_in(file, (*lo, *hi), method, object, args),
            ValueAt::Each {
                list,
                at,
                key,
                loop_at,
            } => self.text_of_each(file, list, *at, key.as_deref(), *loop_at),
            _ => hole(UnreadExpression),
        }
    }

    /// A name a `for…of` head binds (carrick#2093): one text per element of
    /// the list, each at the tuple position or object key the pattern reads,
    /// marked with the loop and the element so the names one loop binds join
    /// element by element ([`Self::joined`]). Every element must state the
    /// part as one text; otherwise the name is the hole of the first element
    /// that does not, and a list no reader states is the hole of why not.
    fn text_of_each(
        &self,
        file: &Path,
        list: &ValueAt,
        at: Option<usize>,
        key: Option<&str>,
        loop_at: u32,
    ) -> Vec<TextValue> {
        let hole = |reason| vec![TextValue::hole(reason)];
        let (items, listed_in, read_from, _reading) = match self.list_elements(file, list) {
            Ok(found) => found,
            Err(reason) => return hole(reason),
        };
        let mut out = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let part = match (at, key, item) {
                (None, None, item) => Some(item.clone()),
                (Some(at), None, ValueAt::Elements(tuple)) => tuple.get(at).cloned(),
                (None, Some(key), ValueAt::Entries(_)) => handed(item, &[key.to_string()]),
                _ => None,
            };
            let Some(part) = part else {
                return hole(guards::HoleReason::UnreadExpression);
            };
            match self.one_text(&listed_in, &part) {
                Ok((text, from)) => {
                    let mut read_from = read_from.clone();
                    note_all(&mut read_from, from);
                    out.push(TextValue::Text {
                        text,
                        read_from,
                        from_type: false,
                        callers: Vec::new(),
                        elements: vec![(loop_at, index)],
                    });
                }
                Err(reason) => return hole(reason),
            }
        }
        if out.is_empty() {
            return hole(guards::HoleReason::UnreadExpression);
        }
        out
    }

    /// The elements of the list a loop reads, the file they are written in,
    /// the declaration it was read through, and the mark that it is being
    /// read: an array literal written in place, or a `const` the binding
    /// table holds as one, where nothing may change it (carrick#2093). The
    /// declaring module never changes it ([`ModuleBinding::list_kept`]), and
    /// an exported list is written `as const`, so no importer changes it
    /// either. Otherwise why the list is unread: a write, else an expression
    /// no reader states, a parameter, or a name declared outside the run.
    #[allow(clippy::type_complexity)]
    fn list_elements(
        &self,
        file: &Path,
        list: &ValueAt,
    ) -> Result<(Vec<ValueAt>, PathBuf, Vec<String>, Option<TextReading<'_>>), guards::HoleReason>
    {
        use guards::HoleReason::*;
        match list {
            ValueAt::Elements(items) => Ok((items.clone(), file.to_path_buf(), Vec::new(), None)),
            ValueAt::Read {
                lo,
                hi,
                member: false,
                ..
            } => {
                let row = self.row_at(file, *lo, *hi, Kind::Ident)?;
                let Some((at, binding, declared)) = self.declared_entry(row) else {
                    return Err(if self.declared_param(row).is_some() {
                        ParameterUnread
                    } else if self.declared_binding(row).is_some() {
                        UnreadExpression
                    } else {
                        self.unread_declaration(row)
                    });
                };
                if binding.unplaceable || !binding.writes.is_empty() {
                    return Err(Reassigned);
                }
                let Some(items) = binding.elements.as_deref() else {
                    return Err(UnreadExpression);
                };
                if binding.kind != BindingKind::Const
                    || !binding.list_kept
                    || (binding.exported && !binding.list_frozen)
                {
                    return Err(UnreadExpression);
                }
                let reading = self.reading_text(&at).ok_or(Recursion)?;
                Ok((
                    items.to_vec(),
                    at.0.to_path_buf(),
                    vec![declared],
                    Some(reading),
                ))
            }
            _ => Err(UnreadExpression),
        }
    }

    /// Whether `value` is a name the compiler types as `string` or as
    /// string literals alone: no `undefined`, no `null`, so a `??` after it
    /// is never taken.
    fn is_non_nullable_text(&self, file: &Path, value: &ValueAt) -> bool {
        let ValueAt::Read {
            lo,
            hi,
            member: false,
            ..
        } = value
        else {
            return false;
        };
        self.row_at(file, *lo, *hi, Kind::Ident)
            .and_then(Row::type_text)
            .is_some_and(|text| {
                text.trim() == "string"
                    || !super::super::site_record::literals_of_type(text).is_empty()
            })
    }

    /// One branch of a choice. A name the compiler types as a union of
    /// literals is each literal (the choice is the code's, not the type's:
    /// the record keeps asking it); anything else reads as it does alone.
    fn text_of_branch(&self, file: &Path, branch: &ValueAt) -> Vec<TextValue> {
        if let ValueAt::Read {
            lo,
            hi,
            member: false,
            ..
        } = branch
            && let Ok(row) = self.row_at(file, *lo, *hi, Kind::Ident)
        {
            let literals = row
                .type_text()
                .map(super::super::site_record::literals_of_type)
                .unwrap_or_default();
            if literals.len() > 1 {
                let read_from: Vec<String> = in_repo_declaration(row).into_iter().collect();
                return literals
                    .into_iter()
                    .map(|text| TextValue::Text {
                        text,
                        read_from: read_from.clone(),
                        from_type: true,
                        callers: Vec::new(),
                        elements: Vec::new(),
                    })
                    .collect();
            }
        }
        self.text_values(file, branch)
    }

    /// Every text `parts`, joined in order, may be (carrick#3191): one per
    /// choice of a text of each part, a part read as a choice
    /// (`${at ?? "/x"}/:id`, a name holding one) giving each of its texts. A
    /// joining that takes a part's hole is that hole, so a consumer that
    /// refuses any hole still does. A part that is only holes is its first
    /// hole, and more than [`MAX_VALUES`] joinings are a
    /// [`guards::HoleReason::FanOutCap`].
    ///
    /// Read through callers (carrick#3504 B10-1b, P5), two values join only
    /// where their chains agree on every definition both fill, and the
    /// joined value carries the union: `pair(a, x)` and `pair(b, y)` state
    /// `a:x` and `b:y`, never `a:y`. The cap counts the joinings within one
    /// chain (B10-2a2, Q6): a chain with more is one cap hole on that chain.
    /// A part that is only holes is one hole per reason and chain, so a
    /// consumer can place one lead per caller.
    fn joined(&self, file: &Path, parts: &[ValueAt]) -> Vec<TextValue> {
        let through_callers = self.callers_mode.get() != CallersRead::Off;
        let mut out = vec![TextValue::stated(String::new(), Vec::new())];
        for part in parts {
            let values = self.text_values(file, part);
            if !values.is_empty()
                && values
                    .iter()
                    .all(|value| matches!(value, TextValue::Hole { .. }))
            {
                if !through_callers {
                    return values.into_iter().take(1).collect();
                }
                let mut holes: Vec<TextValue> = Vec::new();
                for hole in values {
                    if !holes.contains(&hole) {
                        holes.push(hole);
                    }
                }
                return holes;
            }
            if !through_callers {
                let agreeing = out
                    .iter()
                    .map(|head| {
                        values
                            .iter()
                            .filter(|tail| values_agree(head, tail))
                            .count()
                    })
                    .sum::<usize>();
                if agreeing > MAX_VALUES {
                    return vec![TextValue::hole(guards::HoleReason::FanOutCap)];
                }
            }
            let mut next: Vec<TextValue> = Vec::new();
            let mut counts: Vec<(Vec<CallAt>, usize)> = Vec::new();
            for head in &out {
                for tail in &values {
                    if !values_agree(head, tail) {
                        continue;
                    }
                    let callers = chains_joined(head.callers(), tail.callers());
                    count_chain(&mut counts, &callers);
                    let joined = match (head, tail) {
                        (TextValue::Hole { reason, .. }, _)
                        | (_, TextValue::Hole { reason, .. }) => TextValue::Hole {
                            reason: *reason,
                            callers,
                        },
                        (
                            TextValue::Text {
                                text,
                                read_from,
                                from_type,
                                elements,
                                ..
                            },
                            TextValue::Text {
                                text: more,
                                read_from: from,
                                from_type: typed,
                                elements: took,
                                ..
                            },
                        ) => {
                            let mut read_from = read_from.clone();
                            note_all(&mut read_from, from.clone());
                            let mut elements = elements.clone();
                            for element in took {
                                if !elements.contains(element) {
                                    elements.push(*element);
                                }
                            }
                            TextValue::Text {
                                text: format!("{text}{more}"),
                                read_from,
                                from_type: *from_type || *typed,
                                callers,
                                elements,
                            }
                        }
                        // Parts are joined by `pieces_joined`, never here:
                        // a part read as a whole text is never pieced.
                        (TextValue::Pieced(_), _) | (_, TextValue::Pieced(_)) => TextValue::Hole {
                            reason: guards::HoleReason::UnreadExpression,
                            callers,
                        },
                    };
                    if !next.contains(&joined) {
                        next.push(joined);
                    }
                }
            }
            // Read through callers, the cap counts the joinings within one
            // chain, never across chains (carrick#3504 B10-2a2, Q6).
            if through_callers {
                for (chain, count) in counts {
                    if count > MAX_VALUES {
                        next.retain(|value| value.callers() != chain.as_slice());
                        next.push(TextValue::Hole {
                            reason: guards::HoleReason::FanOutCap,
                            callers: chain,
                        });
                    }
                }
            }
            out = next;
        }
        out
    }

    /// `part` read as exactly one text, else why not. A text a caller
    /// decides (one read through callers, carrick#3504 B10-1b) is not what
    /// `part` always is: it is refused as a set is.
    pub(in crate::request_summary) fn one_text(
        &self,
        file: &Path,
        part: &ValueAt,
    ) -> Result<(String, Vec<String>), guards::HoleReason> {
        match self.text_values(file, part).as_slice() {
            [
                TextValue::Text {
                    text,
                    read_from,
                    callers,
                    ..
                },
            ] if callers.is_empty() => Ok((text.clone(), read_from.clone())),
            [TextValue::Hole { reason, .. }] => Err(*reason),
            // A name read as parts is refused (carrick#3504 B10-2a2, Q5):
            // with its first hole's reason, as the whole reader answers it,
            // else as a text a caller decides.
            [TextValue::Pieced(pieced)] => Err(pieced
                .first_hole()
                .unwrap_or(guards::HoleReason::UnreadExpression)),
            _ => Err(guards::HoleReason::UnreadExpression),
        }
    }
}

/// Notes each file of `from` in `read_from`, once.
fn note_all(read_from: &mut Vec<String>, from: Vec<String>) {
    for file in from {
        if !read_from.contains(&file) {
            read_from.push(file);
        }
    }
}

impl<'a> Resolver<'a> {
    /// The binding table's entry for the declaration `row` names, when a
    /// file of the run declares it at module scope or as a local the text
    /// reader reads.
    fn declared_entry(
        &self,
        row: &Row,
    ) -> Option<((&'a Path, u32, String), &'a ModuleBinding, String)> {
        let (path, line, symbol) = self.declared_binding(row)?;
        let binding = self
            .inputs_for(path)
            .files
            .get(path)?
            .module_bindings
            .get(&(line, symbol.clone()))?;
        let declared = super::super::site_record::declaration_path(row.decl_file()?);
        Some(((path, line, symbol), binding, declared))
    }

    /// Why `row`'s declaration has no binding table entry: a parameter of a
    /// file of the run, which only the callers entry point reads
    /// ([`Self::texts_through_callers`]), or a declaration outside the run
    /// (carrick#3795).
    fn unread_declaration(&self, row: &Row) -> guards::HoleReason {
        if self.declared_param(row).is_some() {
            guards::HoleReason::ParameterUnread
        } else {
            guards::HoleReason::OutsideService
        }
    }

    /// A name read at `lo..hi` of `file`: the one literal the compiler types
    /// it as, else the value of the declaration its row names.
    fn text_of_name(&self, file: &Path, lo: u32, hi: u32) -> Vec<TextValue> {
        use guards::HoleReason::*;
        let hole = |reason| vec![TextValue::hole(reason)];
        let row = match self.row_at(file, lo, hi, Kind::Ident) {
            Ok(row) => row,
            Err(reason) => return hole(reason),
        };
        if let Some(text) = typed_as_one_text(row) {
            return vec![TextValue::stated(
                text,
                in_repo_declaration(row).into_iter().collect(),
            )];
        }
        // Read through callers, a parameter is what its callers pass: asked
        // before the binding table, which holds a defaulted parameter as the
        // choice of its default (carrick#3504 B10-1b).
        if self.callers_mode.get() != CallersRead::Off
            && let Some((at, param)) = self.declared_param(row)
        {
            return self.param_texts(at, param, &[], None);
        }
        let Some((at, binding, declared)) = self.declared_entry(row) else {
            return hole(self.unread_declaration(row));
        };
        // A `let` the module writes again (`=`, `??=`, `||=`) holds its
        // initialiser and every value written (carrick#3504 S2b-4, D6): a
        // set. The write census marks every other form of write. A
        // function's own `let` holds, at this read, only the ways whose
        // writes reach it ([`Resolver::written_at`], carrick#3679 S8): their
        // tests are not decided here, so the read is their union. Each
        // source says whether it is a write.
        let mut sources: Vec<(&ValueAt, bool)> = Vec::new();
        let mut reaching_init = true;
        let written = match binding.kind {
            BindingKind::Var => return hole(Reassigned),
            BindingKind::Let if binding.unplaceable => return hole(Reassigned),
            // Two loop heads on one line with one name: no row says which a
            // use means (carrick#2093).
            BindingKind::LoopElement if binding.unplaceable => return hole(Reassigned),
            BindingKind::Let if !binding.writes.is_empty() => {
                let Some(ir) = self.inputs_for(at.0).files.get(at.0) else {
                    return hole(OutsideService);
                };
                let mut starts: Vec<u32> = binding.writes.clone();
                if let Some(ways) = self.written_at(at.0, (at.1, &at.2), lo) {
                    reaching_init = ways.iter().any(|way| way.from == WayFrom::Init);
                    starts = ways
                        .iter()
                        .filter_map(|way| match way.from {
                            WayFrom::Write(start, _) => Some(start),
                            WayFrom::Init | WayFrom::Unwritten => None,
                        })
                        .collect();
                    starts.sort_unstable();
                    starts.dedup();
                }
                for start in starts {
                    match placed_write(ir, start) {
                        Some((_, write)) => sources.push((&write.text, true)),
                        None => return hole(Reassigned),
                    }
                }
                true
            }
            BindingKind::Enum | BindingKind::StaticField | BindingKind::Getter => {
                return hole(UnreadExpression);
            }
            _ => false,
        };
        // A declaration with no initialiser adds no value to a written set.
        match &binding.text {
            Some(text) if reaching_init => sources.insert(0, (text, false)),
            Some(_) => {}
            None if written => {}
            None => return hole(UnreadExpression),
        }
        let Some(_reading) = self.reading_text(&at) else {
            return hole(Recursion);
        };
        let mut out: Vec<TextValue> = Vec::new();
        // A write's value the reader does not state is the reassignment's
        // hole: the name may hold it wherever it is read, so a consumer that
        // reads past other holes (a route's ways, carrick#3191) still stops.
        for (source, is_write) in sources {
            for value in self.text_values(at.0, source) {
                let value = match value {
                    TextValue::Hole { callers, .. } if is_write => TextValue::Hole {
                        reason: Reassigned,
                        callers,
                    },
                    value => value,
                };
                // An empty text names nothing; in a written set it drops out.
                let empty = matches!(&value, TextValue::Text { text, .. } if text.is_empty());
                if (written && empty) || out.contains(&value) {
                    continue;
                }
                out.push(value);
            }
        }
        if out.is_empty() {
            return hole(UnreadExpression);
        }
        for value in &mut out {
            if let TextValue::Text { read_from, .. } = value {
                read_from.retain(|file| *file != declared);
                read_from.insert(0, declared.clone());
            }
        }
        out
    }

    /// An entry read off an enum or a `const` object literal, or an
    /// `as const` object's entry the compiler types as one literal.
    fn text_of_member(
        &self,
        file: &Path,
        (lo, hi): (u32, u32),
        object: &ValueAt,
        key: &ValueAt,
    ) -> Vec<TextValue> {
        use guards::HoleReason::*;
        let hole = |reason| vec![TextValue::hole(reason)];
        let (key, mut read_from) = match self.one_text(file, key) {
            Ok(key) => key,
            Err(reason) => return hole(reason),
        };
        if let Some(out) = self.member_declared(file, (lo, hi), &mut read_from) {
            return out;
        }
        // An object literal written in place (`const { a } = { a: '/x' }`)
        // cannot escape: its last entry with the key is the value.
        if let ValueAt::Entries(entries) = object {
            let value = match self.entry_of(file, entries, &key, true) {
                Ok(Some(value)) => value,
                Ok(None) => return hole(UnreadExpression),
                Err(reason) => return hole(reason),
            };
            let mut out = self.text_values(file, value);
            for entry in &mut out {
                if let TextValue::Text {
                    read_from: from, ..
                } = entry
                {
                    let mut all = read_from.clone();
                    note_all(&mut all, std::mem::take(from));
                    *from = all;
                }
            }
            return out;
        }
        let ValueAt::Read {
            lo,
            hi,
            member: false,
            ..
        } = object
        else {
            return hole(UnreadExpression);
        };
        let row = match self.row_at(file, *lo, *hi, Kind::Ident) {
            Ok(row) => row,
            Err(reason) => return hole(reason),
        };
        // Read through callers, an entry of a parameter (`opts.topic`) is
        // what each caller passes at that key (carrick#3504 B10-1b).
        if self.callers_mode.get() != CallersRead::Off
            && let Some((at, param)) = self.declared_param(row)
        {
            let mut out = self.param_texts(at, param, std::slice::from_ref(&key), None);
            for entry in &mut out {
                if let TextValue::Text {
                    read_from: from, ..
                } = entry
                {
                    let mut all = read_from.clone();
                    note_all(&mut all, std::mem::take(from));
                    *from = all;
                }
            }
            return out;
        }
        let Some((at, binding, declared)) = self.declared_entry(row) else {
            return hole(self.unread_declaration(row));
        };
        let Some(ValueAt::Entries(entries)) = &binding.text else {
            return hole(UnreadExpression);
        };
        // An enum's member is its initialiser; a `const` (or a `let` never
        // assigned again) object's entry is the value of the last entry with
        // the key, where the module only ever reads the object's entries.
        let last = match binding.kind {
            BindingKind::Enum => false,
            BindingKind::StaticField
            | BindingKind::Getter
            | BindingKind::Function
            | BindingKind::LoopElement => {
                return hole(UnreadExpression);
            }
            BindingKind::Var => return hole(Reassigned),
            BindingKind::Let if !binding.writes.is_empty() || binding.unplaceable => {
                return hole(Reassigned);
            }
            BindingKind::Const | BindingKind::Let => {
                if !binding.entries_kept {
                    return hole(UnreadExpression);
                }
                true
            }
        };
        let value = match self.entry_of(at.0, entries, &key, last) {
            Ok(Some(value)) => value,
            Ok(None) => return hole(UnreadExpression),
            Err(reason) => return hole(reason),
        };
        let Some(_reading) = self.reading_text(&at) else {
            return hole(Recursion);
        };
        note_all(&mut read_from, vec![declared]);
        let mut out = self.text_values(at.0, value);
        for entry in &mut out {
            if let TextValue::Text {
                read_from: from, ..
            } = entry
            {
                let mut all = read_from.clone();
                note_all(&mut all, std::mem::take(from));
                *from = all;
            }
        }
        out
    }

    /// What the member read at `lo..hi` of `file` is by its own row: the one
    /// literal the compiler types it as, else a `static` property or a
    /// getter the binding table holds, with the declarations read noted in
    /// `read_from`. `None` where the row says neither.
    fn member_declared(
        &self,
        file: &Path,
        (lo, hi): (u32, u32),
        read_from: &mut Vec<String>,
    ) -> Option<Vec<TextValue>> {
        use guards::HoleReason::*;
        let hole = |reason| Some(vec![TextValue::hole(reason)]);
        let row = self.row_at(file, lo, hi, Kind::Member).ok()?;
        if let Some(text) = typed_as_one_text(row) {
            note_all(read_from, in_repo_declaration(row).into_iter().collect());
            return Some(vec![TextValue::stated(text, read_from.clone())]);
        }
        // A `static` property of a class the module keeps (`C.P`): the row
        // names the property, the binding table its initialiser.
        let (at, binding, declared) = self.declared_entry(row)?;
        if !matches!(binding.kind, BindingKind::StaticField | BindingKind::Getter) {
            return None;
        }
        let Some(text) = &binding.text else {
            return hole(UnreadExpression);
        };
        let Some(_reading) = self.reading_text(&at) else {
            return hole(Recursion);
        };
        note_all(read_from, vec![declared]);
        let mut out = self.text_values(at.0, text);
        for entry in &mut out {
            if let TextValue::Text {
                read_from: from, ..
            } = entry
            {
                let mut all = read_from.clone();
                note_all(&mut all, std::mem::take(from));
                *from = all;
            }
        }
        Some(out)
    }

    /// `this.<field>` read at `lo..hi` of `file` in a member of the class
    /// whose span starts at `class`: what its own row says
    /// ([`Self::member_declared`]), else, read through callers, what each
    /// construction of the class passes where its constructor sets the
    /// field from a parameter ([`Self::field_texts`], carrick#3678 S6-2).
    /// Anything else is the hole a member of no object the reader reads is.
    fn text_of_field(
        &self,
        file: &Path,
        (lo, hi): (u32, u32),
        class: u32,
        field: &str,
    ) -> Vec<TextValue> {
        if let Some(out) = self.member_declared(file, (lo, hi), &mut Vec::new()) {
            return out;
        }
        if self.callers_mode.get() != CallersRead::Off
            && let Some(out) = self.field_texts(file, class, field, None)
        {
            return out;
        }
        vec![TextValue::hole(guards::HoleReason::UnreadExpression)]
    }

    /// What `field` of the class whose span starts at `class` in `file`
    /// holds, read through each construction of the class as
    /// [`Self::param_texts`] reads a parameter through each call: the text
    /// twin of [`Self::field_values`] (carrick#3678 S6-2). Each value
    /// carries the construction it was read at, `fills` naming the
    /// constructor, and goes on through the construction's own callers where
    /// it hands its own parameter on. A class no file constructs is a
    /// [`guards::HoleReason::NameNeverBound`] hole, as a function nothing
    /// calls is.
    ///
    /// `None` where the class is not read through its constructions
    /// ([`Self::field_origin`]), or the constructor sets the field from no
    /// parameter, or with a fallback of its own: the field reads as before.
    fn field_texts(
        &self,
        file: &Path,
        class: u32,
        field: &str,
        read: Option<&(SpanAt, Vec<String>)>,
    ) -> Option<Vec<TextValue>> {
        use guards::HoleReason::*;
        let file: &'a Path = self.inputs.files.get_key_value(file)?.0;
        let (origin, from, built) = self.field_origin(file, class, field)?;
        let (position, keys) = from.text.as_ref()?.plain_param()?;
        let Some(_reading) = self.reading_text(&(file, class, format!("this.{field}"))) else {
            return Some(vec![TextValue::hole(Recursion)]);
        };
        if built.is_empty() {
            return Some(vec![TextValue::hole(NameNeverBound)]);
        }
        let fills = (file.to_path_buf(), format!("{}.constructor", origin.name));
        let mut out: Vec<TextValue> = Vec::new();
        for placed in &built {
            let call = CallAt {
                file: placed.file.to_path_buf(),
                lo: placed.call.site.lo,
                hi: placed.call.site.hi,
                fills: fills.clone(),
            };
            let args = placed.call.passed_texts();
            let values = if args.iter().take(position + 1).any(Option::is_none) {
                vec![TextValue::hole(CallerArgMissing)]
            } else {
                match args
                    .get(position)
                    .and_then(Option::as_ref)
                    .and_then(|term| handed(term, keys))
                {
                    Some(term) => self.text_values(placed.file, &term),
                    None => vec![TextValue::hole(CallerArgMissing)],
                }
            };
            for value in values {
                let value = value.read_at(&call, read);
                if !out.contains(&value) {
                    out.push(value);
                }
            }
        }
        cap_each_chain(&mut out);
        Some(out)
    }

    /// The value of the entry named `key` in `entries` (keys read in
    /// `file`): the last one with the key where `last`, else the first.
    fn entry_of<'v>(
        &self,
        file: &Path,
        entries: &'v [(ValueAt, ValueAt)],
        key: &str,
        last: bool,
    ) -> Result<Option<&'v ValueAt>, guards::HoleReason> {
        let mut found = None;
        for (name, value) in entries {
            let name = match name {
                ValueAt::Lit(name) => name.clone(),
                other => self.one_text(file, other).map(|(name, _)| name)?,
            };
            if name == key {
                found = Some(value);
                if !last {
                    break;
                }
            }
        }
        Ok(found)
    }

    /// `join`, `concat`, `replace`, `replaceAll` and `String.raw` of the
    /// language library, over stated parts.
    fn text_of_built_in(
        &self,
        file: &Path,
        (lo, hi): (u32, u32),
        method: &str,
        object: &ValueAt,
        args: &[ValueAt],
    ) -> Vec<TextValue> {
        use guards::HoleReason::*;
        let hole = |reason| vec![TextValue::hole(reason)];
        let library = self
            .row_at(file, lo, hi, Kind::Call)
            .ok()
            .and_then(|row| row.decl_file())
            .is_some_and(|declared| declared.starts_with("lib:"));
        if !library {
            return hole(UnreadExpression);
        }
        let mut read_from = Vec::new();
        let mut texts = Vec::with_capacity(args.len());
        for arg in args {
            match self.one_text(file, arg) {
                Ok((text, from)) => {
                    texts.push(text);
                    note_all(&mut read_from, from);
                }
                Err(reason) => return hole(reason),
            }
        }
        let part = |value: &ValueAt, read_from: &mut Vec<String>| match self.one_text(file, value) {
            Ok((text, from)) => {
                note_all(read_from, from);
                Ok(text)
            }
            Err(reason) => Err(reason),
        };
        let text = match method {
            "join" => {
                let ValueAt::Elements(elements) = object else {
                    return hole(UnreadExpression);
                };
                let separator = match texts.as_slice() {
                    [] => ",".to_string(),
                    [one] => one.clone(),
                    _ => return hole(UnreadExpression),
                };
                let mut parts = Vec::with_capacity(elements.len());
                for element in elements {
                    match part(element, &mut read_from) {
                        Ok(text) => parts.push(text),
                        Err(reason) => return hole(reason),
                    }
                }
                parts.join(&separator)
            }
            "concat" => match part(object, &mut read_from) {
                Ok(text) => text + &texts.concat(),
                Err(reason) => return hole(reason),
            },
            "replace" | "replaceAll" => {
                let [pattern, replacement] = texts.as_slice() else {
                    return hole(UnreadExpression);
                };
                // A replacement's `$` patterns read the match: not stated.
                if replacement.contains('$') {
                    return hole(UnreadExpression);
                }
                match part(object, &mut read_from) {
                    Ok(text) if method == "replace" => {
                        text.replacen(pattern.as_str(), replacement, 1)
                    }
                    Ok(text) => text.replace(pattern.as_str(), replacement),
                    Err(reason) => return hole(reason),
                }
            }
            "raw" => match part(object, &mut read_from) {
                Ok(text) => text,
                Err(reason) => return hole(reason),
            },
            _ => return hole(UnreadExpression),
        };
        vec![TextValue::stated(text, read_from)]
    }

    /// What the parameter `param`, declared at `at`, holds `more` keys
    /// further in, read at each caller of its definition from the one
    /// caller index (carrick#3504 B10-1b, P6 and P7): what the caller passes
    /// at the parameter's position, through its keys, read where the caller
    /// is written and tagged with that call; the default where the caller
    /// passes nothing there. A caller passing on a parameter being read
    /// unchanged adds nothing (soundness rule 7).
    ///
    /// Read as parts (carrick#3504 B10-2a2), `read` is the parameter read
    /// and its keys: each part a caller passes goes through it. Values
    /// across callers are never capped; [`MAX_VALUES`] counts within one
    /// chain (Q6).
    fn param_texts(
        &self,
        at: (&'a Path, u32, String),
        param: &ParamAt,
        more: &[String],
        read: Option<&(SpanAt, Vec<String>)>,
    ) -> Vec<TextValue> {
        use guards::HoleReason::*;
        let hole = |reason| vec![TextValue::hole(reason)];
        if param.collides || param.written {
            return hole(Reassigned);
        }
        let Some(function) = param.function.as_deref() else {
            return hole(CallbackParameter);
        };
        let Some(_reading) = self.reading_text(&at) else {
            return hole(Recursion);
        };
        let file = at.0;
        let keys: Vec<String> = param.keys.iter().chain(more).cloned().collect();
        let callers = self.callers().callers_of(file, function).to_vec();
        let mut out: Vec<TextValue> = Vec::new();
        if callers.is_empty() {
            out.push(TextValue::hole(NameNeverBound));
            for value in self.default_texts(file, function, param.index, &keys) {
                if !out.contains(&value) {
                    out.push(value);
                }
            }
            return out;
        }
        let fills = (file.to_path_buf(), function.to_string());
        for caller in callers {
            let call = CallAt {
                file: caller.file.to_path_buf(),
                lo: caller.call.site.lo,
                hi: caller.call.site.hi,
                fills: fills.clone(),
            };
            let args = caller.call.passed_texts();
            // A spread at or before the position moves it; a JSX element
            // passes its props as no positional argument, so it is never an
            // omitted one that takes the default (P2, P6).
            let values = if caller.call.jsx
                || args.iter().take(param.index + 1).any(Option::is_none)
            {
                hole(CallerArgMissing)
            } else {
                let written = args.get(param.index).and_then(Option::as_ref);
                match written.and_then(|term| handed(term, &keys)) {
                    _ if written.is_some_and(|term| self.passes_on(caller.file, term)) => continue,
                    Some(term) if self.passes_on(caller.file, &term) => continue,
                    Some(term) => self.text_values(caller.file, &term),
                    // Nothing passed there: the default, else nothing read.
                    None => {
                        let defaults = self.default_texts(file, function, param.index, &keys);
                        if defaults.is_empty() {
                            hole(CallerArgMissing)
                        } else {
                            defaults
                        }
                    }
                }
            };
            for value in values {
                let value = value.read_at(&call, read);
                if !out.contains(&value) {
                    out.push(value);
                }
            }
        }
        cap_each_chain(&mut out);
        out
    }

    /// What the default of the parameter at `index` of the definition
    /// `function` of `file` states at `keys` (a whole parameter's default
    /// read down the keys, or a destructured key's own), read where the
    /// definition is written. A default read from another parameter is
    /// none a caller passes here.
    fn default_texts(
        &self,
        file: &'a Path,
        function: &str,
        index: usize,
        keys: &[String],
    ) -> Vec<TextValue> {
        let Some((path, ir)) = self.function(file, function) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (position, at, text) in &ir.param_default_texts {
            if *position != index || !keys.starts_with(at) {
                continue;
            }
            let Some(term) = handed(text, &keys[at.len()..]) else {
                continue;
            };
            if self.param_read(path, &term).is_some() {
                out.push(TextValue::hole(guards::HoleReason::CallerArgMissing));
                continue;
            }
            out.extend(self.text_values(path, &term));
        }
        out
    }

    /// Whether `term`, written in `file`, is a parameter being read for its
    /// text: a caller that hands it on unchanged inside a cycle.
    fn passes_on(&self, file: &Path, term: &ValueAt) -> bool {
        self.param_read(file, term)
            .is_some_and(|id| self.texts_reading.borrow().contains(&id))
    }

    /// The parameter declaration `term`, written in `file`, reads where it
    /// is a name read of one: its file, line and name.
    fn param_read(&self, file: &Path, term: &ValueAt) -> Option<(PathBuf, u32, String)> {
        let ValueAt::Read {
            lo,
            hi,
            member: false,
            ..
        } = term
        else {
            return None;
        };
        let row = self.row_at(file, *lo, *hi, Kind::Ident).ok()?;
        let ((path, line, symbol), _) = self.declared_param(row)?;
        Some((path.to_path_buf(), line, symbol))
    }

    /// Marks the declaration `at` as being read for its text, until the
    /// guard drops; `None` where it already is (a value that reads itself).
    fn reading_text(&self, at: &(&'a Path, u32, String)) -> Option<TextReading<'_>> {
        let id = (at.0.to_path_buf(), at.1, at.2.clone());
        // The borrow ends before any guard exists: a guard built and dropped
        // under it (`then_some` builds its argument eagerly) would release
        // the mark while the set is still borrowed, and panic.
        let marked = self.texts_reading.borrow_mut().insert(id.clone());
        marked.then(|| TextReading {
            reading: &self.texts_reading,
            id,
        })
    }

    /// The one text `value`, written in `file` (as `inputs` keys it), always
    /// is ([`Self::one_text`]); otherwise why it is none (carrick#3688 U2).
    pub(in crate::request_summary) fn text_of_value(
        &self,
        file: &Path,
        value: &ValueAt,
    ) -> Result<String, Stop> {
        self.one_text(file, value)
            .map(|(text, _)| text)
            .map_err(Stop::from)
    }

    /// The one text `expr`, written in the repo file `file` (relative to
    /// `root`), always is, with the files whose declarations it read;
    /// otherwise why it is none (carrick#3688 U2). A file that is none of
    /// the run's is `outside_service`.
    pub(in crate::request_summary) fn text_read(
        &self,
        root: &Path,
        file: &str,
        expr: &Expr,
    ) -> Result<(String, Vec<String>), Stop> {
        let absolute = root.join(file);
        let (path, ir) = self
            .inputs_for(&absolute)
            .files
            .get_key_value(&absolute)
            .ok_or(Stop::OutsideService)?;
        let value = text_of(expr, &|lo| ir.discovery_start(lo));
        self.one_text(path, &value).map_err(Stop::from)
    }
}

impl Resolver<'_> {
    /// Every text `expr`, written in the repo file `file` (relative to
    /// `root`), may be, never collapsed: a choice (`?:`, `??`, `||`) is each
    /// of its branches, and a branch the code does not state is a
    /// [`TextValue::Hole`]. Empty when the file is not one of the run's.
    pub(in crate::request_summary) fn text_set(
        &self,
        root: &Path,
        file: &str,
        expr: &Expr,
    ) -> Vec<TextValue> {
        let absolute = root.join(file);
        let Some((path, ir)) = self.inputs_for(&absolute).files.get_key_value(&absolute) else {
            return Vec::new();
        };
        let value = text_of(expr, &|lo| ir.discovery_start(lo));
        self.text_values(path, &value)
    }

    /// Every text the entry `key` of the object `expr` holds may be, never
    /// collapsed (carrick#3504 S5a): `expr` is a name the binding table
    /// holds as an object literal. An empty set says the object, which the
    /// module only reads through its entries, or hands to the call starting
    /// at `call` alone, has no such key. `None` where
    /// `expr` is no such name (a parameter, a call result, a name outside the
    /// run): the object is not read at all.
    pub(in crate::request_summary) fn keyed_text_set(
        &self,
        root: &Path,
        file: &str,
        expr: &Expr,
        key: &str,
        call: u32,
    ) -> Option<ArgTexts> {
        let absolute = root.join(file);
        let (path, ir) = self.inputs_for(&absolute).files.get_key_value(&absolute)?;
        let object = text_of(expr, &|lo| ir.discovery_start(lo));
        // The census numbers spans as discovery does.
        let call = ir.discovery_start(call);
        let ValueAt::Read {
            lo,
            hi,
            member: false,
            ..
        } = &object
        else {
            return None;
        };
        let row = self.row_at(path, *lo, *hi, Kind::Ident).ok()?;
        let (at, binding, _) = self.declared_entry(row)?;
        let Some(ValueAt::Entries(entries)) = &binding.text else {
            return None;
        };
        let closed = match binding.kind {
            BindingKind::Const => true,
            BindingKind::Let => binding.writes.is_empty() && !binding.unplaceable,
            BindingKind::Var
            | BindingKind::Enum
            | BindingKind::StaticField
            | BindingKind::Getter
            | BindingKind::Function
            | BindingKind::LoopElement => false,
        };
        // The entries are the literal's where the module only reads them, or
        // hands the object on to the one call this entry is read for.
        let kept = binding.entries_kept
            || (binding.entries_handed && binding.handed.iter().all(|at| *at == call));
        // A closed object whose entries are not kept (exported, handed to
        // more than one call, or written by the module) is an object the
        // resolver does not read (R3).
        if closed && !kept {
            return Some(ArgTexts::Unread(Stop::UnreadExpression));
        }
        if closed && kept {
            return Some(ArgTexts::Values(
                match self.entry_of(at.0, entries, key, true) {
                    Ok(None) => Vec::new(),
                    Ok(Some(value)) => match self.reading_text(&at) {
                        Some(_reading) => self.text_values(at.0, value),
                        None => vec![TextValue::hole(guards::HoleReason::Recursion)],
                    },
                    Err(reason) => vec![TextValue::hole(reason)],
                },
            ));
        }
        let member = ValueAt::Member {
            lo: 0,
            hi: 0,
            object: Box::new(object),
            key: Box::new(ValueAt::Lit(key.to_string())),
        };
        Some(ArgTexts::Values(self.text_values(path, &member)))
    }

    /// The parts of the text `expr`, written in the repo file `file`
    /// (relative to `root`), states: a template's or a `+` chain's pieces in
    /// order, each with the file of the outermost declaration it was read
    /// through (`None` for text written in place). `None` unless every part
    /// is exactly one text.
    pub(in crate::request_summary) fn text_parts(
        &self,
        root: &Path,
        file: &str,
        expr: &Expr,
    ) -> Option<Vec<(String, Option<String>)>> {
        fn leaves<'v>(value: &'v ValueAt, out: &mut Vec<&'v ValueAt>) {
            match value {
                ValueAt::Concat(parts) => parts.iter().for_each(|part| leaves(part, out)),
                other => out.push(other),
            }
        }
        let absolute = root.join(file);
        let (path, ir) = self.inputs_for(&absolute).files.get_key_value(&absolute)?;
        let value = text_of(expr, &|lo| ir.discovery_start(lo));
        let mut parts = Vec::new();
        leaves(&value, &mut parts);
        let mut out = Vec::new();
        for part in parts {
            match part {
                // An empty quasi writes nothing.
                ValueAt::Lit(text) if text.is_empty() => {}
                ValueAt::Lit(text) => out.push((text.clone(), None)),
                // A text a caller decides is no part the text always states.
                other => match self.text_values(path, other).as_slice() {
                    [
                        TextValue::Text {
                            text,
                            read_from,
                            callers,
                            ..
                        },
                    ] if callers.is_empty() => {
                        out.push((text.clone(), read_from.first().cloned()));
                    }
                    _ => return None,
                },
            }
        }
        Some(out)
    }
}

/// A declaration being read for its text; dropping it releases the mark.
struct TextReading<'r> {
    reading: &'r RefCell<HashSet<(PathBuf, u32, String)>>,
    id: (PathBuf, u32, String),
}

impl Drop for TextReading<'_> {
    fn drop(&mut self) {
        self.reading.borrow_mut().remove(&self.id);
    }
}

/// The one string literal the compiler types `row` as.
fn typed_as_one_text(row: &Row) -> Option<String> {
    match row
        .type_text()
        .map(super::super::site_record::literals_of_type)
        .unwrap_or_default()
        .as_slice()
    {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// The file `row` is declared in, as a record names a declaration, when it
/// is a file of the repo.
fn in_repo_declaration(row: &Row) -> Option<String> {
    row.decl_file()
        .filter(|declared| super::super::site_record::in_repo(declared))
        .map(super::super::site_record::declaration_path)
}

#[cfg(test)]
mod tests {
    //! The text reader on the forms the name fold read before it moved onto
    //! the resolver (carrick#3504 S2). Fixture: `resolve-text` (its README
    //! is the answer key).

    use super::super::super::fixture_walk::{Walked, walked};
    use super::*;

    /// What the probe constant `name` of `file` reads as.
    fn probe(resolver: &Resolver<'_>, on: &Walked, file: &str, name: &str) -> Vec<TextValue> {
        let path = on.path(&format!("services/web/{file}"));
        let text = on.inputs.files[&path]
            .module_bindings
            .iter()
            .find(|((_, symbol), _)| symbol == name)
            .and_then(|(_, binding)| binding.text.clone())
            .unwrap_or_else(|| panic!("{name} is declared with an initialiser in {file}"));
        resolver.text_values(&path, &text)
    }

    /// What the argument of the one call of `callee` in `blocks.ts` reads as.
    fn blocks_read(resolver: &Resolver<'_>, on: &Walked, callee: &str) -> Vec<TextValue> {
        let path = on.path("services/web/src/blocks.ts");
        let found: Vec<ValueAt> = super::super::row_calls::every_call(&on.inputs.files)
            .into_iter()
            .filter(|(file, call, _)| *file == path && call.callee == callee)
            .filter_map(|(_, call, _)| call.arg_texts.first().cloned().flatten())
            .collect();
        assert_eq!(found.len(), 1, "{callee}: {found:?}");
        resolver.text_values(&path, &found[0])
    }

    /// The texts of a read, or the failure when it holds a hole.
    fn texts_of(got: &[TextValue]) -> Result<Vec<&str>, String> {
        got.iter()
            .map(|value| match value {
                TextValue::Text { text, .. } => Ok(text.as_str()),
                hole => Err(format!("{hole:?}")),
            })
            .collect()
    }

    /// carrick#3794 T2: a local declared in a block, at module scope or in a
    /// function written inside a body reads as its initialiser's text, a
    /// `let` written again in a block as every value it holds.
    #[test]
    fn a_block_or_nested_local_reads_as_its_initialiser_text() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        for (callee, want) in [
            ("p_if", vec!["order.created"]),
            ("p_try", vec!["https://example.test/try"]),
            ("p_case", vec!["case.a"]),
            ("p_bare", vec!["bare"]),
            ("p_loop_body", vec!["loop.body"]),
            ("p_module_block", vec!["module.block"]),
            ("p_callback", vec!["cb.top"]),
            ("p_callback_block", vec!["cb.block"]),
            ("p_handler", vec!["handler.top"]),
            ("p_moved", vec!["a", "b"]),
        ] {
            let got = blocks_read(&resolver, &on, callee);
            assert_eq!(texts_of(&got), Ok(want), "{callee}: {got:?}");
        }
    }

    /// A declaration the table refuses, or whose write is never placed, still
    /// reads as before: F4 gives the refusals a true word.
    #[test]
    fn a_name_the_table_refuses_still_reads_as_today() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        for callee in ["p_shadow", "p_shadow_outer", "p_catch"] {
            let got = blocks_read(&resolver, &on, callee);
            assert!(
                matches!(
                    got.as_slice(),
                    [TextValue::Hole {
                        reason: guards::HoleReason::OutsideService,
                        ..
                    }]
                ),
                "{callee}: {got:?}"
            );
        }
        // A nested `let` written again: its write is never placed.
        let got = blocks_read(&resolver, &on, "p_nested_moved");
        assert!(
            matches!(
                got.as_slice(),
                [TextValue::Hole {
                    reason: guards::HoleReason::Reassigned,
                    ..
                }]
            ),
            "{got:?}"
        );
        // A loop head is the one block-scoped name the text reader reads
        // (carrick#2093): its list's one element.
        let looped = blocks_read(&resolver, &on, "p_loop");
        assert!(
            matches!(looped.as_slice(), [TextValue::Text { text, .. }] if text == "x"),
            "{looped:?}"
        );
    }

    /// The texts and holes the probe call `callee` in `src/loops.ts` reads
    /// as (carrick#2093): the texts sorted, then each hole's reason.
    fn loop_probe(
        resolver: &Resolver<'_>,
        on: &Walked,
        callee: &str,
    ) -> (Vec<String>, Vec<guards::HoleReason>) {
        let path = on.path("services/web/src/loops.ts");
        let found: Vec<ValueAt> = super::super::row_calls::every_call(&on.inputs.files)
            .into_iter()
            .filter(|(file, call, _)| *file == path && call.callee == callee)
            .filter_map(|(_, call, _)| call.arg_texts.first().cloned().flatten())
            .collect();
        assert_eq!(found.len(), 1, "{callee}: {found:?}");
        let mut texts = Vec::new();
        let mut holes = Vec::new();
        for value in resolver.text_values(&path, &found[0]) {
            match value {
                TextValue::Text { text, .. } => texts.push(text),
                TextValue::Hole { reason, .. } => holes.push(reason),
                TextValue::Pieced(pieced) => panic!("{callee}: pieced: {pieced:?}"),
            }
        }
        texts.sort();
        (texts, holes)
    }

    /// carrick#2093: a name a `for…of` head binds over a list the code
    /// states, and that nothing changes, is each element: a name, a tuple's
    /// positions (read together, element by element), an object's key, two
    /// nested loops (every pairing), an exported `as const` list and a list
    /// a function body declares. Fixture `resolve-text` (`loops.ts`).
    #[test]
    fn a_loop_head_over_a_stated_list_is_each_element() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let texts = |callee: &str, want: &[&str]| {
            let want: Vec<String> = want.iter().map(|text| text.to_string()).collect();
            assert_eq!(
                loop_probe(&resolver, &on, callee),
                (want, Vec::new()),
                "{callee}"
            );
        };
        texts("p_each", &["large", "small"]);
        texts("p_tuple", &["f1:t1", "f2:t2"]);
        texts("p_keyed", &["n1", "n2"]);
        texts("p_two", &["a1b1", "a1b2", "a2b1", "a2b2"]);
        texts("p_frozen", &["apply", "create"]);
        texts("p_local", &["one", "two"]);
    }

    /// carrick#2093: a loop head whose list the code does not state, or may
    /// change, is one hole of why, and never `outside_service`: the head is
    /// declared in the file. A list exported without `as const`, pushed to,
    /// handed to a call, held in a `let` or built with a spread; a call's
    /// result; a parameter; a `let` head; a `for…in` head; a pattern with a
    /// default.
    #[test]
    fn a_loop_head_over_a_list_no_reader_states_is_a_hole_of_why() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        use guards::HoleReason::{ParameterUnread, UnreadExpression};
        for (callee, reason) in [
            ("p_loose", UnreadExpression),
            ("p_pushed", UnreadExpression),
            ("p_handed", UnreadExpression),
            ("p_let_list", UnreadExpression),
            ("p_spread", UnreadExpression),
            ("p_call", UnreadExpression),
            ("p_param", ParameterUnread),
            ("p_let_head", UnreadExpression),
            ("p_in", UnreadExpression),
            ("p_default", UnreadExpression),
        ] {
            assert_eq!(
                loop_probe(&resolver, &on, callee),
                (Vec::new(), vec![reason]),
                "{callee}"
            );
        }
    }

    /// A choice reads every branch, never collapsed: a branch the code does
    /// not state is a hole beside the others, a name typed as a union of
    /// literals is each literal, and a nested choice flattens.
    #[test]
    fn an_alt_reads_every_branch() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let alt = "src/alt.ts";
        let texts = |name: &str| -> (Vec<String>, usize) {
            let got = probe(&resolver, &on, alt, name);
            let holes = got
                .iter()
                .filter(|value| matches!(value, TextValue::Hole { .. }))
                .count();
            let mut texts: Vec<String> = got
                .into_iter()
                .filter_map(|value| match value {
                    TextValue::Text { text, .. } => Some(text),
                    TextValue::Hole { .. } => None,
                    TextValue::Pieced(pieced) => panic!("pieced outside its entry: {pieced:?}"),
                })
                .collect();
            texts.sort();
            (texts, holes)
        };
        assert_eq!(texts("p_coalesce"), (vec!["/fallback".to_string()], 1));
        assert_eq!(texts("p_or"), (vec!["/or".to_string()], 1));
        // carrick#3835: a left typed as one non-nullable text is the whole
        // value of `??`; `||` keeps both sides (as `p_coalesce` does for a nullable left).
        assert_eq!(texts("p_nullish_typed"), (vec!["/api".to_string()], 0));
        assert_eq!(
            texts("p_or_typed"),
            (vec!["/api".to_string(), "/falsy".to_string()], 0)
        );
        assert_eq!(
            texts("p_cond"),
            (vec!["/api".to_string(), "/other".to_string()], 0)
        );
        assert_eq!(
            texts("p_chain"),
            (vec!["/api".to_string(), "/last".to_string()], 1)
        );
        assert_eq!(
            texts("p_union"),
            (
                vec!["/u/a".to_string(), "/u/b".to_string(), "/u/c".to_string()],
                0
            )
        );
    }

    /// Every form the name fold read: a `const`, a `let` never assigned
    /// again, an enum (and a const enum), a constant object's entry by name
    /// and by a computed key, an `as const` entry, `join`, `concat`,
    /// `replace`, `replaceAll`, `String.raw`, a template, `+`, and a `const`
    /// in a top-level function's body. Each states the declaring file. The
    /// forms the fold refused stay holes.
    #[test]
    fn the_text_reader_reads_every_form_the_name_fold_read() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let forms = "src/forms.ts";
        let read = |name: &str, want: &str| {
            let got = probe(&resolver, &on, forms, name);
            match got.as_slice() {
                [
                    TextValue::Text {
                        text, read_from, ..
                    },
                ] => {
                    assert_eq!(text, want, "{name}");
                    // Literals alone read no declaration.
                    let literal = ["p_join_default", "p_replace_all"].contains(&name);
                    assert!(
                        if literal {
                            read_from.is_empty()
                        } else {
                            matches!(read_from.as_slice(), [file] if file.ends_with(forms))
                        },
                        "{name}: {read_from:?}"
                    );
                }
                other => panic!("{name}: {other:?}"),
            }
        };
        read("p_const", "/api");
        read("p_let", "/late");
        read("p_enum", "/items");
        read("p_const_enum", "/health");
        read("p_entry", "/things");
        read("p_computed_key", "/other");
        read("p_as_const", "/frozen");
        read("p_join", "/api/items/all");
        read("p_join_default", "a,b");
        read("p_concat", "/api/x/y");
        read("p_replace", "/v1");
        read("p_replace_all", "/b/b");
        read("p_raw", "/api/\\d");
        read("p_template", "/api//items/tail");
        read("p_plus", "/api//health");
        read("p_local_join", "/api/local");

        let held = |file: &str, name: &str, reason: guards::HoleReason| {
            assert_eq!(
                probe(&resolver, &on, file, name),
                [TextValue::hole(reason)],
                "{name}"
            );
        };
        use guards::HoleReason::{Reassigned, UnreadExpression};
        // A `let` assigned again holds its initialiser and each write
        // (carrick#3504 S2b-4, D6): two texts, never one.
        let written: Vec<String> = probe(&resolver, &on, "src/refuse.ts", "p_written")
            .into_iter()
            .map(|value| match value {
                TextValue::Text { text, .. } => text,
                hole => panic!("p_written: {hole:?}"),
            })
            .collect();
        assert_eq!(written, ["/before", "/after"]);
        held("src/refuse.ts", "p_written_entry", UnreadExpression);
        held("src/refuse.ts", "p_exported", UnreadExpression);
        held("src/refuse.ts", "p_var", Reassigned);
        held("src/refuse.ts", "p_pushed", UnreadExpression);
        held("src/refuse.ts", "p_dollar", UnreadExpression);
        held("src/refuse.ts", "p_param", UnreadExpression);
        // `eval` in the module may write any binding.
        held("src/evalfile.ts", "p_eval", Reassigned);
        // An inner scope that declares the name again blocks nothing.
        let shadow = probe(&resolver, &on, "src/shadow.ts", "p_shadow");
        assert!(
            matches!(shadow.as_slice(), [TextValue::Text { text, .. }] if text == "/outer"),
            "{shadow:?}"
        );
    }

    /// A constant that reads itself is a recursion hole. The reader marks
    /// the declaration once; the second read finds the mark and must not
    /// release it while the mark set is still borrowed (a panic, "RefCell
    /// already borrowed", once #3649 made such reads reachable).
    #[test]
    fn a_constant_that_reads_itself_is_a_recursion_hole() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let got = probe(&resolver, &on, "src/selfread.ts", "p_self");
        assert!(
            got.iter().any(|value| matches!(
                value,
                TextValue::Hole {
                    reason: guards::HoleReason::Recursion,
                    ..
                }
            )),
            "{got:?}"
        );
        // The mark is released: reading again gives the same answer.
        assert_eq!(probe(&resolver, &on, "src/selfread.ts", "p_self"), got);
    }

    /// What the argument of the one call of `callee` in `order.ts` reads as
    /// (carrick#3679 S8 T1): each text, and each hole as `hole:<reason>`,
    /// sorted. `None` without the built compiler engine.
    fn order_reads(callee: &str) -> Option<Vec<String>> {
        let on = walked("resolve-text", "services/web", "web", &[], true)?;
        let resolver = Resolver::new(&on.inputs, &[]);
        let path = on.path("services/web/src/order.ts");
        let found: Vec<ValueAt> = super::super::row_calls::every_call(&on.inputs.files)
            .into_iter()
            .filter(|(file, call, _)| *file == path && call.callee == callee)
            .filter_map(|(_, call, _)| call.arg_texts.first().cloned().flatten())
            .collect();
        assert_eq!(found.len(), 1, "{callee}: {found:?}");
        let mut out: Vec<String> = resolver
            .text_values(&path, &found[0])
            .into_iter()
            .map(|value| match value {
                TextValue::Text { text, .. } => text,
                TextValue::Hole { reason, .. } => format!("hole:{reason:?}"),
                TextValue::Pieced(pieced) => panic!("{callee}: pieced {pieced:?}"),
            })
            .collect();
        out.sort();
        Some(out)
    }

    /// A function's own `let` written after the read never held that value
    /// there (carrick#3679 S8 T1): the read holds only its initialiser.
    #[test]
    fn a_write_after_the_read_is_not_what_the_read_holds() {
        let Some(got) = order_reads("p_after") else {
            return;
        };
        assert_eq!(got, ["/before"]);
    }

    /// A branch that writes the local and then returns passes nothing on:
    /// the read after it holds only what reaches it past the branch.
    #[test]
    fn a_write_in_a_branch_that_returns_never_reaches_a_later_read() {
        let Some(got) = order_reads("p_exit") else {
            return;
        };
        assert_eq!(got, ["/a"]);
    }

    /// Guard: either branch may run, so both values reach the read.
    #[test]
    fn both_branches_are_read() {
        let Some(got) = order_reads("p_both") else {
            return;
        };
        assert_eq!(got, ["/a", "/b"]);
    }

    /// Guard: a write in a loop refuses the local in the flow, and the
    /// text reader keeps its set (the initialiser and every write).
    #[test]
    fn a_local_written_in_a_loop_keeps_every_value() {
        let Some(got) = order_reads("p_looped") else {
            return;
        };
        assert_eq!(got, ["/a", "hole:Reassigned"]);
    }

    /// Guard: `??=` adds a way and keeps the one that reaches it.
    #[test]
    fn a_logical_assign_adds_to_what_reaches_it() {
        let Some(got) = order_reads("p_nullish") else {
            return;
        };
        assert_eq!(got, ["/a", "/b"]);
    }

    /// Guard: a write the flow does not place (in an `if` test) refuses the
    /// local, and the text reader keeps its set.
    #[test]
    fn a_write_the_flow_does_not_place_keeps_every_value() {
        let Some(got) = order_reads("p_hidden") else {
            return;
        };
        assert_eq!(got, ["/a", "/b"]);
    }

    // Parameters read through their callers (carrick#3504 B10-1b). Fixture
    // `resolve-text`, `callers.ts`; its README holds the key. Proof each
    // ran: `walked` is `None` without the built compiler engine and the test
    // returns early, which CI refuses (`fixture_walk::sidecar`).

    /// The file of the probes, and the term the one call of `callee` in it
    /// hands its first argument as.
    fn probe_arg(on: &Walked, callee: &str) -> (PathBuf, ValueAt) {
        arg_in(on, "src/callers.ts", callee)
    }

    /// The file `file` of the service, and the term the one call of
    /// `callee` in it hands its first argument as.
    fn arg_in(on: &Walked, file: &str, callee: &str) -> (PathBuf, ValueAt) {
        let path = on.path(&format!("services/web/{file}"));
        let mut found: Vec<ValueAt> = super::super::super::row_calls::every_call(&on.inputs.files)
            .into_iter()
            .filter(|(file, call, _)| *file == path && call.callee == callee)
            .filter_map(|(_, call, _)| call.arg_texts.first().cloned().flatten())
            .collect();
        assert_eq!(found.len(), 1, "{callee}: {found:?}");
        (path, found.remove(0))
    }

    /// The call `at` names, written as its callee and its first argument: a
    /// literal as written, another term `_`, a spread `...`.
    fn call_named(on: &Walked, at: &CallAt) -> String {
        super::super::super::row_calls::every_call(&on.inputs.files)
            .into_iter()
            .find(|(file, call, _)| {
                *file == at.file && call.site.lo == at.lo && call.site.hi == at.hi
            })
            .map(|(_, call, _)| {
                let arg = match call.arg_texts.first() {
                    Some(Some(ValueAt::Lit(text))) => format!("{text:?}"),
                    Some(Some(_)) => "_".to_string(),
                    Some(None) => "...".to_string(),
                    None => String::new(),
                };
                format!("{}({arg})", call.callee)
            })
            .unwrap_or_else(|| panic!("no call at {at:?}"))
    }

    /// Each value: its text, or `hole:` and its word, with the calls it was
    /// read through by name, innermost first.
    fn flat(on: &Walked, values: &[TextValue]) -> Vec<(String, Vec<String>)> {
        values
            .iter()
            .map(|value| {
                let said = match value {
                    TextValue::Text { text, .. } => text.clone(),
                    TextValue::Hole { reason, .. } => {
                        format!("hole:{}", Stop::from(*reason).as_str())
                    }
                    TextValue::Pieced(pieced) => panic!("pieced outside its entry: {pieced:?}"),
                };
                let calls = value
                    .callers()
                    .iter()
                    .map(|at| call_named(on, at))
                    .collect();
                (said, calls)
            })
            .collect()
    }

    /// What the probe `callee`'s argument reads as through callers, sorted.
    fn through_callers(
        resolver: &Resolver<'_>,
        on: &Walked,
        callee: &str,
    ) -> Vec<(String, Vec<String>)> {
        through_callers_in(resolver, on, "src/callers.ts", callee)
    }

    /// What the probe `callee`'s argument in `file` reads as through
    /// callers, sorted.
    fn through_callers_in(
        resolver: &Resolver<'_>,
        on: &Walked,
        file: &str,
        callee: &str,
    ) -> Vec<(String, Vec<String>)> {
        let (path, term) = arg_in(on, file, callee);
        let mut got = flat(on, &resolver.texts_through_callers(&path, &term));
        got.sort();
        got
    }

    /// `(text, calls)` rows, for comparing with [`through_callers`].
    fn rows(want: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = want
            .iter()
            .map(|(said, calls)| {
                (
                    said.to_string(),
                    calls.iter().map(|call| call.to_string()).collect(),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// Pin (passes before B10-1b): every existing entry point reads a
    /// parameter as it did, hole word included, with no calls on any value.
    #[test]
    fn reading_without_callers_answers_a_parameter_as_before() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let (path, one) = probe_arg(&on, "p_one");
        assert_eq!(
            resolver.text_values(&path, &one),
            [TextValue::hole(guards::HoleReason::ParameterUnread)]
        );
        assert_eq!(
            resolver.one_text(&path, &one),
            Err(guards::HoleReason::ParameterUnread)
        );
        let (path, defaulted) = probe_arg(&on, "p_defaulted");
        let got = resolver.text_values(&path, &defaulted);
        // In order: the choice of a defaulted parameter, a hole and then
        // its default.
        assert_eq!(
            flat(&on, &got),
            [
                ("hole:unread_expression".to_string(), Vec::new()),
                ("fallback".to_string(), Vec::new())
            ],
            "{got:?}"
        );
    }

    /// carrick#3795: a value of a file in no service of the run still stops
    /// as `outside_service`; only a parameter of the run is `parameter_unread`.
    #[test]
    fn a_file_outside_the_run_stops_as_outside_service() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let expr = Expr::Invalid(swc_ecma_ast::Invalid {
            span: swc_common::DUMMY_SP,
        });
        assert_eq!(
            resolver
                .text_read(&on.repo, "services/nowhere/src/none.ts", &expr)
                .map_err(|stop| stop.as_str()),
            Err("outside_service")
        );
    }

    /// carrick#3504 B10-1b: a parameter reads as each text its callers
    /// pass, each with the call it was read at.
    #[test]
    fn a_parameter_reads_as_the_text_each_caller_passes() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        assert_eq!(
            through_callers(&resolver, &on, "p_one"),
            rows(&[("order.created", &["one(\"order.created\")"])])
        );
        assert_eq!(
            through_callers(&resolver, &on, "p_two"),
            rows(&[
                ("order.created", &["two(\"order.created\")"]),
                ("order.updated", &["two(\"order.updated\")"]),
                ("hole:caller_arg_missing", &["two(...)"]),
            ])
        );
        let send: Vec<String> = through_callers(&resolver, &on, "p_send")
            .into_iter()
            .map(|(said, calls)| {
                assert_eq!(calls.len(), 1, "{calls:?}");
                said
            })
            .collect();
        assert_eq!(send, ["bus.sent"]);
    }

    /// carrick#3504 B10-1b: a parameter handed on to another function
    /// carries every call it was read through, innermost first.
    #[test]
    fn a_parameter_passed_on_carries_each_call_it_was_read_through() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        assert_eq!(
            through_callers(&resolver, &on, "p_outer"),
            rows(&[("deep", &["outer(_)", "middle(\"deep\")"])])
        );
    }

    // Fields a constructor sets from its parameters (carrick#3678 S6-2).
    // Fixture `resolve-text`, `fields.ts`; its README holds the key.

    /// carrick#3678 S6-2: read through callers, `this.<field>` set from a
    /// constructor parameter is what each construction of the class passes
    /// there, read through the construction (and on through its own
    /// callers, as a parameter is). A field the compiler types as one
    /// literal is that literal; a class refused its constructions, and a
    /// field with a fallback, read as before.
    #[test]
    fn a_field_set_from_a_constructor_parameter_reads_each_construction_s_caller_text() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let read = |callee: &str| through_callers_in(&resolver, &on, "src/fields.ts", callee);
        assert_eq!(
            read("p_field_param"),
            rows(&[(
                "orders.a3.*",
                &["ParamHub(_)", "subscribe(\"orders.a3.*\")"]
            )])
        );
        assert_eq!(
            read("p_field_lit"),
            rows(&[("orders.lit", &["LitHub(\"orders.lit\")"])])
        );
        assert_eq!(
            read("p_field_hub"),
            rows(&[("orders.hub", &["InnerSub(_)", "subscribe(\"orders.hub\")"])])
        );
        assert_eq!(
            read("p_field_assign"),
            rows(&[("orders.assign", &["AssignHub(\"orders.assign\")"])])
        );
        assert_eq!(
            read("p_field_twice"),
            rows(&[
                ("orders.one", &["TwiceHub(\"orders.one\")"]),
                ("orders.two", &["TwiceHub(\"orders.two\")"]),
            ])
        );
        assert_eq!(read("p_field_typed"), rows(&[("orders.typed", &[])]));
        assert_eq!(
            read("p_field_spread"),
            rows(&[("hole:unread_expression", &[])])
        );
        assert_eq!(
            read("p_field_fallback"),
            rows(&[("hole:unread_expression", &[])])
        );
    }

    /// carrick#3678 S6-2, R7 (pin, passes before S6-2): with no callers
    /// read, every field probe reads exactly as before.
    #[test]
    fn a_field_reads_as_before_without_callers() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        for callee in [
            "p_field_param",
            "p_field_lit",
            "p_field_hub",
            "p_field_assign",
            "p_field_twice",
            "p_field_spread",
            "p_field_fallback",
        ] {
            let (path, term) = arg_in(&on, "src/fields.ts", callee);
            assert_eq!(
                resolver.text_values(&path, &term),
                [TextValue::hole(guards::HoleReason::UnreadExpression)],
                "{callee}"
            );
            assert_eq!(
                resolver.one_text(&path, &term),
                Err(guards::HoleReason::UnreadExpression),
                "{callee}"
            );
        }
        let (path, term) = arg_in(&on, "src/fields.ts", "p_field_typed");
        assert_eq!(
            flat(&on, &resolver.text_values(&path, &term)),
            [("orders.typed".to_string(), Vec::new())]
        );
    }

    /// carrick#3678 S6-2: read as parts, a field is passed through its
    /// construction as a parameter is through a call: the field read is the
    /// first hop, the construction's own parameter the next.
    #[test]
    fn a_field_read_as_parts_goes_through_its_construction() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let (path, term) = arg_in(&on, "src/fields.ts", "p_field_param");
        let got = resolver.pieces_through_callers(&path, &term);
        let [value] = got.as_slice() else {
            panic!("{got:?}");
        };
        assert_eq!(
            described(&on, value),
            [(
                "orders.a3.*".to_string(),
                vec![
                    "ParamHub(_)".to_string(),
                    "subscribe(\"orders.a3.*\")".to_string()
                ]
            )]
        );
        // The first hop reads the field where the labelled call writes it.
        assert_eq!(
            Some(value.parts[0].through[0].read.clone()),
            span_of(&path, &term)
        );
    }

    /// carrick#3504 B10-1b, P5: the parts of one text join only where they
    /// were read at the same call of the function they fill.
    #[test]
    fn parts_of_one_text_pair_by_the_calls_they_were_read_at() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        assert_eq!(
            through_callers(&resolver, &on, "p_pair"),
            rows(&[("a:x", &["pair(\"a\")"]), ("b:y", &["pair(\"b\")"])])
        );
    }

    /// carrick#3504 B10-1b: a destructured key of a parameter, and an entry
    /// read off one, are each caller's entry at that key.
    #[test]
    fn a_destructured_key_and_a_member_of_a_parameter_read_each_caller_s_entry() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        assert_eq!(
            through_callers(&resolver, &on, "p_keyed"),
            rows(&[("jobs.run", &["keyed(_)"])])
        );
        assert_eq!(
            through_callers(&resolver, &on, "p_member"),
            rows(&[("member.topic", &["member(_)"])])
        );
    }

    /// carrick#3504 B10-1b: a caller that passes nothing at the position (or
    /// the key) reads the default there, tagged with that caller.
    #[test]
    fn an_omitted_argument_reads_the_default_at_that_caller() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        assert_eq!(
            through_callers(&resolver, &on, "p_defaulted"),
            rows(&[
                ("fallback", &["defaulted()"]),
                ("given", &["defaulted(\"given\")"]),
            ])
        );
        assert_eq!(
            through_callers(&resolver, &on, "p_keyed_default"),
            rows(&[("jobs.default", &["keyedDefault(_)"])])
        );
    }

    /// carrick#3504 B10-1b, P6: each parameter no caller states says why.
    #[test]
    fn each_parameter_hole_says_why() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let hole = |callee: &str, word: &str| {
            assert_eq!(
                through_callers(&resolver, &on, callee),
                rows(&[(&format!("hole:{word}"), &[])]),
                "{callee}"
            );
        };
        hole("p_uncalled", "name_never_bound");
        hole("p_callback", "callback_parameter");
        hole("p_written", "reassigned");
        hole("p_shared", "reassigned");
        assert!(through_callers(&resolver, &on, "p_two").contains(&(
            "hole:caller_arg_missing".to_string(),
            vec!["two(...)".to_string()]
        )));
    }

    /// carrick#3504 B10-1b, P6 and soundness rule 7: a caller that hands
    /// the parameter on unchanged inside a cycle adds nothing; a changed
    /// value in a cycle is a recursion hole.
    #[test]
    fn a_cycle_that_passes_the_parameter_on_adds_nothing() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        assert_eq!(
            through_callers(&resolver, &on, "p_loop"),
            rows(&[("once", &["loop(\"once\")"])])
        );
        assert_eq!(
            through_callers(&resolver, &on, "p_ping"),
            rows(&[("m", &["ping(\"m\")"])])
        );
        assert_eq!(
            through_callers(&resolver, &on, "p_grow"),
            rows(&[
                ("seed", &["grow(\"seed\")"]),
                ("hole:recursion", &["grow(_)"]),
            ])
        );
    }

    /// carrick#3504 B10-1b, P5: a text read as one part of a larger one
    /// (a `join`'s element, a computed key) never takes a caller's text,
    /// and never comes back as a text with no calls.
    #[test]
    fn a_text_read_as_one_part_refuses_a_caller_s_text() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        for callee in ["p_joined", "p_lookup"] {
            assert_eq!(
                through_callers(&resolver, &on, callee),
                rows(&[("hole:unread_expression", &[])]),
                "{callee}"
            );
        }
    }

    /// carrick#3504 B10-1b, P5: holes read at different callers stay apart,
    /// alone and as a part of a larger text, so a consumer can place one
    /// lead per caller.
    #[test]
    fn holes_from_different_callers_stay_apart() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let (path, term) = probe_arg(&on, "p_runtime");
        let joined = ValueAt::Concat(vec![term.clone(), ValueAt::Lit("/x".to_string())]);
        for value in [term, joined] {
            let got = resolver.texts_through_callers(&path, &value);
            assert_eq!(got.len(), 2, "{got:?}");
            assert!(
                got.iter().all(
                    |value| matches!(value, TextValue::Hole { callers, .. } if callers.len() == 1)
                ),
                "{got:?}"
            );
            assert_ne!(got[0].callers(), got[1].callers(), "{got:?}");
        }
    }

    // A name read through callers as pieces (carrick#3504 B10-2a2).
    // Fixture `resolve-text`, `pieces.ts`; its README holds the key.

    /// The file of the piece probes, and the term the one call of `callee`
    /// in it hands its first argument as.
    fn piece_arg(on: &Walked, callee: &str) -> (PathBuf, ValueAt) {
        arg_in(on, "src/pieces.ts", callee)
    }

    /// Q6: values across callers are never capped, one per chain.
    #[test]
    fn seventeen_callers_give_seventeen_values() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let (path, term) = piece_arg(&on, "p_wide");
        let whole = resolver.texts_through_callers(&path, &term);
        let mut texts: Vec<String> = whole
            .iter()
            .map(|value| match value {
                TextValue::Text { text, callers, .. } if callers.len() == 1 => text.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        texts.sort();
        let want: Vec<String> = (1..=17).map(|n| format!("w{n:02}")).collect();
        assert_eq!(texts, want);
        let mut pieced: Vec<String> = resolver
            .pieces_through_callers(&path, &term)
            .iter()
            .map(|value| {
                assert_eq!(value.callers.len(), 1, "{value:?}");
                match described(&on, value).as_slice() {
                    [(said, hops)] if hops.len() == 1 => said.clone(),
                    other => panic!("{other:?}"),
                }
            })
            .collect();
        pieced.sort();
        assert_eq!(pieced, want);
    }

    /// Each part of a pieced value: its text, or `hole:` and its word, and
    /// the calls of the parameters it was passed through, innermost first.
    fn described(on: &Walked, value: &PiecedText) -> Vec<(String, Vec<String>)> {
        value
            .parts
            .iter()
            .map(|part| {
                let said = match &part.value {
                    PartValue::Lit { text, .. } => text.clone(),
                    PartValue::Hole(reason) => format!("hole:{}", Stop::from(*reason).as_str()),
                };
                let hops = part
                    .through
                    .iter()
                    .map(|hop| call_named(on, &hop.call))
                    .collect();
                (said, hops)
            })
            .collect()
    }

    /// Q2, Q3: a template over a parameter keeps its literal part, and each
    /// caller's term is a part passed through the parameter at that call: a
    /// literal, or a hole at the caller's term.
    #[test]
    fn a_template_over_a_parameter_keeps_its_parts_and_each_caller_s_term() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let (path, term) = piece_arg(&on, "p_url");
        let mut got: Vec<Vec<(String, Vec<String>)>> = resolver
            .pieces_through_callers(&path, &term)
            .iter()
            .map(|value| {
                assert_eq!(value.callers.len(), 1, "{value:?}");
                let [_, part] = value.parts.as_slice() else {
                    panic!("{value:?}");
                };
                assert_eq!(part.through[0].call, value.callers[0], "{value:?}");
                if matches!(part.value, PartValue::Hole(_)) {
                    // The hole is at the caller's term, in the caller's file.
                    let at = part.at.as_ref().expect("a hole at the caller's term");
                    assert_eq!(at.file, part.through[0].call.file);
                    assert!(
                        at.lo >= part.through[0].call.lo && at.hi <= part.through[0].call.hi,
                        "{value:?}"
                    );
                }
                described(&on, value)
            })
            .collect();
        got.sort();
        let part = |said: &str, hops: &[&str]| {
            (
                said.to_string(),
                hops.iter().map(|hop| hop.to_string()).collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            got,
            [
                vec![part("/users/", &[]), part("42", &["user(\"42\")"]),],
                vec![
                    part("/users/", &[]),
                    part("hole:unread_expression", &["user(_)"]),
                ],
            ]
        );
    }

    /// Q3: a term not split by its own template, a local or a parameter is
    /// one part: a choice is one hole at its span, an imported constant one
    /// literal at its read, never its initialiser's parts.
    #[test]
    fn a_term_not_split_by_its_own_template_is_one_part() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let one = |callee: &str| -> (PathBuf, ValueAt, PiecedText) {
            let (path, term) = piece_arg(&on, callee);
            let mut got = resolver.pieces_through_callers(&path, &term);
            assert_eq!(got.len(), 1, "{callee}: {got:?}");
            (path, term, got.remove(0))
        };
        let (path, term, env) = one("p_env");
        let ValueAt::Concat(terms) = &term else {
            panic!("{term:?}");
        };
        let choice = terms
            .iter()
            .find(|term| matches!(term, ValueAt::Alt { .. }))
            .expect("the choice");
        assert_eq!(
            described(&on, &env),
            [
                ("hole:unread_expression".to_string(), Vec::new()),
                ("/items".to_string(), Vec::new()),
            ]
        );
        assert_eq!(env.parts[0].at, span_of(&path, choice));
        assert!(env.parts[0].at.is_some());

        let (path, term, base) = one("p_base");
        let ValueAt::Concat(terms) = &term else {
            panic!("{term:?}");
        };
        let read = terms
            .iter()
            .find(|term| matches!(term, ValueAt::Read { .. }))
            .expect("the base's read");
        assert_eq!(
            described(&on, &base),
            [
                ("https://api.example.test".to_string(), Vec::new()),
                ("/things".to_string(), Vec::new()),
            ]
        );
        assert_eq!(base.parts[0].at, span_of(&path, read));
        assert!(base.parts[0].at.is_some());
    }

    /// Q3: in pieces mode a parameter is read before the one literal its
    /// type may be, so the part is passed through it; read whole, it is the
    /// type's literal, through no call.
    #[test]
    fn a_parameter_typed_as_one_literal_is_read_as_a_parameter() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let (path, term) = piece_arg(&on, "p_typed");
        let got: Vec<Vec<(String, Vec<String>)>> = resolver
            .pieces_through_callers(&path, &term)
            .iter()
            .map(|value| described(&on, value))
            .collect();
        assert_eq!(
            got,
            [vec![(
                "jobs.run".to_string(),
                vec!["typed(\"jobs.run\")".to_string()]
            )]]
        );
        assert_eq!(
            flat(&on, &resolver.texts_through_callers(&path, &term)),
            [("jobs.run".to_string(), Vec::new())]
        );
    }

    /// Q5: a pieced value is refused by `one_text` with its first hole's
    /// word, and with `unread_expression` where its parts are all literals a
    /// caller decides; a part of one text never takes a caller's text.
    #[test]
    fn a_pieced_value_is_refused_with_its_first_hole_s_word() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let in_pieces = |path: &Path, term: &ValueAt| {
            let _mode = CallersMode::set(&resolver.callers_mode, CallersRead::Pieces);
            resolver.one_text(path, term)
        };
        let (path, uncalled) = probe_arg(&on, "p_uncalled");
        assert_eq!(
            in_pieces(&path, &uncalled),
            Err(guards::HoleReason::NameNeverBound)
        );
        let (path, one) = probe_arg(&on, "p_one");
        assert_eq!(
            in_pieces(&path, &one),
            Err(guards::HoleReason::UnreadExpression)
        );
        for callee in ["p_joined", "p_lookup"] {
            let (path, term) = probe_arg(&on, callee);
            let got: Vec<Vec<(String, Vec<String>)>> = resolver
                .pieces_through_callers(&path, &term)
                .iter()
                .map(|value| described(&on, value))
                .collect();
            assert_eq!(
                got,
                [vec![("hole:unread_expression".to_string(), Vec::new())]],
                "{callee}"
            );
        }
    }

    /// The B10-2a1 caveat: a local splits into its initialiser's parts only
    /// where the request summaries read it as those parts. Read at the
    /// labelled call's own function, every probe spells what the summaries
    /// read at the call's argument.
    #[test]
    fn a_local_splits_only_where_the_summaries_read_it_as_its_initialiser() {
        let Some(on) = walked("resolve-text", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        for (callee, parts) in [("p_local", 2), ("p_target", 2), ("p_renamed", 1)] {
            let (path, term) = piece_arg(&on, callee);
            let ValueAt::Read { lo, hi, .. } = term else {
                panic!("{callee}: {term:?}");
            };
            let summaries = on.inputs.files[&path]
                .arg_pieces
                .get(&(lo, hi))
                .cloned()
                .unwrap_or_else(|| panic!("{callee}: no pieces at the argument"));
            let got = resolver.pieces_through_callers(&path, &term);
            assert_eq!(got.len(), 1, "{callee}: {got:?}");
            assert_eq!(got[0].parts.len(), parts, "{callee}: {got:?}");
            assert_eq!(
                super::super::super::decided_sites::rendered(&resolver, &got[0], 0),
                summaries,
                "{callee}: {got:?}"
            );
        }
    }
}
