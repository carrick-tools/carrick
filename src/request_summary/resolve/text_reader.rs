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
    },
    /// The name is none the reader states.
    Hole(guards::HoleReason),
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
            return vec![TextValue::Hole(guards::HoleReason::FanOutCap)];
        }
        let out = self.text_in(file, value);
        if self.open.get().0 == 1 && self.capped.get() {
            return vec![TextValue::Hole(guards::HoleReason::FanOutCap)];
        }
        out
    }

    fn text_in(&self, file: &Path, value: &ValueAt) -> Vec<TextValue> {
        use guards::HoleReason::*;
        let hole = |reason| vec![TextValue::Hole(reason)];
        match value {
            ValueAt::Lit(text) => vec![TextValue::Text {
                text: text.clone(),
                read_from: Vec::new(),
            }],
            ValueAt::Concat(parts) => {
                let mut text = String::new();
                let mut read_from = Vec::new();
                for part in parts {
                    match self.one_text(file, part) {
                        Ok((part, from)) => {
                            text.push_str(&part);
                            note_all(&mut read_from, from);
                        }
                        Err(reason) => return hole(reason),
                    }
                }
                vec![TextValue::Text { text, read_from }]
            }
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
                    })
                    .collect();
            }
        }
        self.text_values(file, branch)
    }

    /// `part` read as exactly one text, else why not.
    fn one_text(
        &self,
        file: &Path,
        part: &ValueAt,
    ) -> Result<(String, Vec<String>), guards::HoleReason> {
        match self.text_values(file, part).as_slice() {
            [TextValue::Text { text, read_from }] => Ok((text.clone(), read_from.clone())),
            [TextValue::Hole(reason)] => Err(*reason),
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

    /// A name read at `lo..hi` of `file`: the one literal the compiler types
    /// it as, else the value of the declaration its row names.
    fn text_of_name(&self, file: &Path, lo: u32, hi: u32) -> Vec<TextValue> {
        use guards::HoleReason::*;
        let hole = |reason| vec![TextValue::Hole(reason)];
        let Some(row) = self.row_at(file, lo, hi, Kind::Ident) else {
            return hole(RowsMissing);
        };
        if let Some(text) = typed_as_one_text(row) {
            return vec![TextValue::Text {
                text,
                read_from: in_repo_declaration(row).into_iter().collect(),
            }];
        }
        let Some((at, binding, declared)) = self.declared_entry(row) else {
            return hole(OutsideService);
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
        for source in sources {
            for value in self.text_values(at.0, source) {
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
        let hole = |reason| vec![TextValue::Hole(reason)];
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
            return vec![TextValue::Text { text, read_from }];
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
        let Some((at, binding, declared)) = self.declared_entry(row) else {
            return hole(OutsideService);
        };
        let Some(ValueAt::Entries(entries)) = &binding.text else {
            return hole(UnreadExpression);
        };
        // An enum's member is its initialiser; a `const` (or a `let` never
        // assigned again) object's entry is the value of the last entry with
        // the key, where the module only ever reads the object's entries.
        let last = match binding.kind {
            BindingKind::Enum => false,
            BindingKind::StaticField | BindingKind::Getter => return hole(UnreadExpression),
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
        let hole = |reason| vec![TextValue::Hole(reason)];
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
        vec![TextValue::Text { text, read_from }]
    }

    /// Marks the declaration `at` as being read for its text, until the
    /// guard drops; `None` where it already is (a value that reads itself).
    fn reading_text(&self, at: &(&'a Path, u32, String)) -> Option<TextReading<'_>> {
        let id = (at.0.to_path_buf(), at.1, at.2.clone());
        self.texts_reading
            .borrow_mut()
            .insert(id.clone())
            .then_some(TextReading {
                reading: &self.texts_reading,
                id,
            })
    }

    /// The one text `expr`, written in the repo file `file` (relative to
    /// `root`), always is, with the files whose declarations it read.
    /// `None` unless it is exactly one text.
    pub(in crate::request_summary) fn text_read(
        &self,
        root: &Path,
        file: &str,
        expr: &Expr,
    ) -> Option<(String, Vec<String>)> {
        let absolute = root.join(file);
        let (path, ir) = self.inputs_for(&absolute).files.get_key_value(&absolute)?;
        let value = text_of(expr, &|lo| ir.discovery_start(lo));
        match self.text_values(path, &value).as_slice() {
            [TextValue::Text { text, read_from }] => Some((text.clone(), read_from.clone())),
            _ => None,
        }
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
                other => match self.text_values(path, other).as_slice() {
                    [TextValue::Text { text, read_from }] => {
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
                .filter(|value| matches!(value, TextValue::Hole(_)))
                .count();
            let mut texts: Vec<String> = got
                .into_iter()
                .filter_map(|value| match value {
                    TextValue::Text { text, .. } => Some(text),
                    TextValue::Hole(_) => None,
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
                [TextValue::Text { text, read_from }] => {
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
                [TextValue::Hole(reason)],
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
}
