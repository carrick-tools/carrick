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
    },
    /// The name is none the reader states, for `reason`, read through
    /// `callers` as a text is.
    Hole {
        reason: guards::HoleReason,
        callers: Vec<CallAt>,
    },
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
            TextValue::Text { callers, .. } | TextValue::Hole { callers, .. } => callers,
        }
    }

    /// This value, read one caller further out: `at` goes first on its
    /// chain.
    fn read_at(mut self, at: &CallAt) -> Self {
        match &mut self {
            TextValue::Text { callers, .. } | TextValue::Hole { callers, .. } => {
                callers.insert(0, at.clone());
            }
        }
        self
    }
}

/// Whether two chains agree on every definition both fill (carrick#3504
/// B10-1b, P5): parts read at different calls of one function never join.
fn chains_agree(head: &[CallAt], tail: &[CallAt]) -> bool {
    head.iter()
        .all(|a| tail.iter().filter(|b| b.fills == a.fills).all(|b| b == a))
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

/// What `term`, handed to a parameter, holds down `keys`: an object literal's
/// last entry with each key (`None` where it writes none: the key is
/// omitted), else a read of the key off the term, as
/// [`Resolver::keyed_text_set`] reads one.
fn handed(term: &ValueAt, keys: &[String]) -> Option<ValueAt> {
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

/// Turns on [`Resolver::texts_through_callers`]'s mode until it drops, then
/// puts back what was there.
struct CallersMode<'r> {
    mode: &'r Cell<bool>,
    before: bool,
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
        if self.open.get().0 == 1 && self.capped.get() {
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
    #[allow(dead_code)]
    pub(in crate::request_summary) fn texts_through_callers(
        &self,
        file: &Path,
        value: &ValueAt,
    ) -> Vec<TextValue> {
        let _mode = CallersMode {
            mode: &self.callers_mode,
            before: self.callers_mode.replace(true),
        };
        self.text_values(file, value)
    }

    fn text_in(&self, file: &Path, value: &ValueAt) -> Vec<TextValue> {
        use guards::HoleReason::*;
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
            ValueAt::Alt { branches } => branches
                .iter()
                .flat_map(|branch| self.text_of_branch(file, branch))
                .collect(),
            ValueAt::Member {
                lo,
                hi,
                object,
                key,
            } => self.text_of_member(file, (*lo, *hi), object, key),
            ValueAt::BuiltIn {
                lo,
                hi,
                method,
                object,
                args,
            } => self.text_of_built_in(file, (*lo, *hi), method, object, args),
            _ => hole(UnreadExpression),
        }
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
            && let Some(row) = self.row_at(file, *lo, *hi, Kind::Ident)
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
    /// `a:x` and `b:y`, never `a:y`. The cap counts the joinings that agree.
    /// A part that is only holes is one hole per reason and chain, so a
    /// consumer can place one lead per caller.
    fn joined(&self, file: &Path, parts: &[ValueAt]) -> Vec<TextValue> {
        let through_callers = self.callers_mode.get();
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
            let agreeing = out
                .iter()
                .map(|head| {
                    values
                        .iter()
                        .filter(|tail| chains_agree(head.callers(), tail.callers()))
                        .count()
                })
                .sum::<usize>();
            if agreeing > MAX_VALUES {
                return vec![TextValue::hole(guards::HoleReason::FanOutCap)];
            }
            let mut next: Vec<TextValue> = Vec::new();
            for head in &out {
                for tail in &values {
                    if !chains_agree(head.callers(), tail.callers()) {
                        continue;
                    }
                    let callers = chains_joined(head.callers(), tail.callers());
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
                                ..
                            },
                            TextValue::Text {
                                text: more,
                                read_from: from,
                                from_type: typed,
                                ..
                            },
                        ) => {
                            let mut read_from = read_from.clone();
                            note_all(&mut read_from, from.clone());
                            TextValue::Text {
                                text: format!("{text}{more}"),
                                read_from,
                                from_type: *from_type || *typed,
                                callers,
                            }
                        }
                    };
                    if !next.contains(&joined) {
                        next.push(joined);
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
        let Some(row) = self.row_at(file, lo, hi, Kind::Ident) else {
            return hole(RowsMissing);
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
        if self.callers_mode.get()
            && let Some((at, param)) = self.declared_param(row)
        {
            return self.param_texts(at, param, &[]);
        }
        let Some((at, binding, declared)) = self.declared_entry(row) else {
            return hole(self.unread_declaration(row));
        };
        // A `let` the module writes again (`=`, `??=`, `||=`) holds its
        // initialiser and every value written (carrick#3504 S2b-4, D6): a
        // set. The write census marks every other form of write.
        let mut sources: Vec<&ValueAt> = Vec::new();
        let written = match binding.kind {
            BindingKind::Var => return hole(Reassigned),
            BindingKind::Let if binding.unplaceable => return hole(Reassigned),
            BindingKind::Let if !binding.writes.is_empty() => {
                let Some(ir) = self.inputs_for(at.0).files.get(at.0) else {
                    return hole(OutsideService);
                };
                for start in &binding.writes {
                    match placed_write(ir, *start) {
                        Some((_, write)) => sources.push(&write.text),
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
            Some(text) => sources.insert(0, text),
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
        let writes_from = sources.len() - if written { binding.writes.len() } else { 0 };
        for (index, source) in sources.into_iter().enumerate() {
            for value in self.text_values(at.0, source) {
                let value = match value {
                    TextValue::Hole { callers, .. } if index >= writes_from => TextValue::Hole {
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
        if let Some(row) = self.row_at(file, lo, hi, Kind::Member)
            && let Some(text) = typed_as_one_text(row)
        {
            note_all(
                &mut read_from,
                in_repo_declaration(row).into_iter().collect(),
            );
            return vec![TextValue::stated(text, read_from)];
        }
        // A `static` property of a class the module keeps (`C.P`): the row
        // names the property, the binding table its initialiser.
        if let Some(row) = self.row_at(file, lo, hi, Kind::Member)
            && let Some((at, binding, declared)) = self.declared_entry(row)
            && matches!(binding.kind, BindingKind::StaticField | BindingKind::Getter)
        {
            let Some(text) = &binding.text else {
                return hole(UnreadExpression);
            };
            let Some(_reading) = self.reading_text(&at) else {
                return hole(Recursion);
            };
            note_all(&mut read_from, vec![declared]);
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
        let Some(row) = self.row_at(file, *lo, *hi, Kind::Ident) else {
            return hole(RowsMissing);
        };
        // Read through callers, an entry of a parameter (`opts.topic`) is
        // what each caller passes at that key (carrick#3504 B10-1b).
        if self.callers_mode.get()
            && let Some((at, param)) = self.declared_param(row)
        {
            let mut out = self.param_texts(at, param, std::slice::from_ref(&key));
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
            BindingKind::StaticField | BindingKind::Getter | BindingKind::Function => {
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
    fn param_texts(
        &self,
        at: (&'a Path, u32, String),
        param: &ParamAt,
        more: &[String],
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
                let value = value.read_at(&call);
                if !out.contains(&value) {
                    out.push(value);
                }
            }
        }
        if out.len() > MAX_VALUES {
            return hole(FanOutCap);
        }
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
        let row = self.row_at(file, *lo, *hi, Kind::Ident)?;
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
        let row = self.row_at(path, *lo, *hi, Kind::Ident)?;
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
            | BindingKind::Function => false,
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
        for callee in ["p_shadow", "p_shadow_outer", "p_loop", "p_catch"] {
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
                })
                .collect();
            texts.sort();
            (texts, holes)
        };
        assert_eq!(texts("p_coalesce"), (vec!["/fallback".to_string()], 1));
        assert_eq!(texts("p_or"), (vec!["/or".to_string()], 1));
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

    // Parameters read through their callers (carrick#3504 B10-1b). Fixture
    // `resolve-text`, `callers.ts`; its README holds the key. Proof each
    // ran: `walked` is `None` without the built compiler engine and the test
    // returns early, which CI refuses (`fixture_walk::sidecar`).

    /// The file of the probes, and the term the one call of `callee` in it
    /// hands its first argument as.
    fn probe_arg(on: &Walked, callee: &str) -> (PathBuf, ValueAt) {
        let path = on.path("services/web/src/callers.ts");
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
        let (path, term) = probe_arg(on, callee);
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
}
