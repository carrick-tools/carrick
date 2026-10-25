//! The parts a file is asked in: windows of its lines, each with the
//! declarations it uses (carrick#2308).
//!
//! A file whose whole answer does not fit one response is asked again in
//! parts. A part does not repeat the file. It shows a window of consecutive
//! lines, the opening and closing lines of every construct the window was cut
//! out of, and the declarations the window refers to, each line under a
//! number that counts the lines the part shows ([`ShownLines::prompt_line`]),
//! so an edit elsewhere in the file moves no number in the part (carrick#2794).
//!
//! Everything here is a function of the file's text, its listed candidates
//! and the limits: no I/O, no name of a library, a framework or a file, and
//! the same parts on every scan.
//!
//! **Where a file can be cut.** Between siblings of a list, and nowhere else:
//! the items of the module, the statements of a function, method, arrow or
//! namespace body, the members of a class, the elements of an array literal,
//! the properties of an object literal, the statements of a block, a loop, an
//! `if` or `else`, a `try`, `catch` or `finally` and a `switch` case, and the
//! body of a function called where it is written. No list is chosen by a name.
//!
//! **Windows are line ranges.** An item runs from the line after the sibling
//! before it ends to the line it ends on, so a comment travels with the item
//! under it, and siblings that end on one line are one item. The first item of
//! a list starts where the list's owner starts and its last item ends where
//! the owner ends, so the items of a list are the owner's lines, each once.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::Path;

use swc_common::{BytePos, GLOBALS, Globals, Mark, Span, Spanned};
use swc_ecma_ast::*;
use swc_ecma_transforms_base::resolver;
use swc_ecma_visit::{Visit, VisitMutWith, VisitWith};

use crate::agents::file_orchestrator::ListedCandidate;
use crate::binding_scope::{BindingKey, ident_key};
use crate::swc_scanner::SWC_SPAN_BASE;

#[cfg(test)]
#[path = "../../tests/support/shapes.rs"]
mod shapes;

/// A declaration over this many bytes is shown by its first line only: the
/// cap an attached wrapper module is held to.
pub const CONTEXT_DECLARATION_MAX: usize = crate::agents::file_orchestrator::WRAPPER_SNIPPET_MAX;

/// Two runs of shown lines this close together are shown as one: the lines
/// between them cost less than the line that would say they are missing.
/// Only while those lines hold at most [`SHOWN_GAP_MAX_BYTES`]: a gap of one
/// long line is not cheap, and filling it could carry the very item a window
/// was cut to leave out (carrick#2309).
const SHOWN_GAP_FILLED: u32 = 2;

/// The most bytes of the lines between two runs that are shown to join them.
const SHOWN_GAP_MAX_BYTES: usize = 200;

/// What a window may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes of source a window holds.
    pub window_bytes: usize,
    /// The most bytes of declarations shown beside a window.
    pub context_bytes: usize,
    /// The most candidates a window holds.
    pub candidates: usize,
}

/// A run of lines, first and last, counted from 1 as the prompt numbers them.
pub type Lines = (u32, u32);

/// The opening and closing lines of one construct a window was cut out of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enclosing {
    pub opening: Lines,
    pub closing: Lines,
}

/// One part of a file: a window of its lines and what is shown beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePart {
    /// The window.
    pub first_line: u32,
    pub last_line: u32,
    /// The candidates whose span starts on a line of the window, as positions
    /// in the prompt's listing, in the order the prompt lists them.
    pub candidates: Vec<usize>,
    /// Every construct the window was cut out of, outermost first.
    pub enclosing: Vec<Enclosing>,
    /// The declarations the window uses, in source order.
    pub context: Vec<Lines>,
    /// The items the window holds, as runs of siblings in source order.
    runs: Vec<Run>,
    /// Every run of lines the part shows, in source order
    /// ([`SourceCut::shown_runs`]).
    shown_runs: Vec<Lines>,
}

/// The lines one part shows, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownLines {
    /// The part's window.
    pub window: Lines,
    /// Every run of lines shown: the window, the lines of the constructs it
    /// was cut out of, and the declarations it uses.
    pub runs: Vec<Lines>,
    /// Whether a line between two runs says which lines are not shown.
    ///
    /// The cloud's pre-call gate strips the line numbers back off only while
    /// nearly every line of the fence carries one (carrick-cloud#2266), so
    /// the lines that carry none stay under one in ten of the lines shown.
    /// A part that cannot keep under that writes none: the numbers on either
    /// side of a gap already say what is missing.
    pub markers: bool,
}

impl ShownLines {
    /// The number the prompt gives `file_line`, or `None` when it is not
    /// shown (carrick#2794).
    ///
    /// A part numbers the lines it shows, not the lines of the file: the
    /// first shown line is 1, each next line in a run is the next number, and
    /// each gap between two runs skips exactly one number. Nothing in the
    /// numbering depends on where in the file a run is, so an edit above or
    /// between the runs changes no number and the part's prompt stays the
    /// same bytes. This and [`Self::file_line`] are the one definition of the
    /// numbering: the prompt is written with the first and its answer is read
    /// with the second.
    pub fn prompt_line(&self, file_line: u32) -> Option<u32> {
        let mut next = 1;
        for (first, last) in &self.runs {
            if (*first..=*last).contains(&file_line) {
                return Some(next + (file_line - first));
            }
            next += (last - first + 1) + 1;
        }
        None
    }

    /// The file line a prompt number names, or `None` for a number that is
    /// not a shown line: zero or below, a skipped gap number, or one past the
    /// last line.
    pub fn file_line(&self, prompt_line: i64) -> Option<u32> {
        if prompt_line <= 0 {
            return None;
        }
        let mut next: i64 = 1;
        for (first, last) in &self.runs {
            let len = i64::from(last - first + 1);
            if (next..next + len).contains(&prompt_line) {
                return Some(first + u32::try_from(prompt_line - next).ok()?);
            }
            next += len + 1;
        }
        None
    }

    /// How many lines are shown.
    pub fn line_count(&self) -> usize {
        self.runs
            .iter()
            .map(|(first, last)| (last - first + 1) as usize)
            .sum()
    }

    /// Whether `line` is shown.
    pub fn holds(&self, line: u32) -> bool {
        self.runs
            .iter()
            .any(|(first, last)| (*first..=*last).contains(&line))
    }
}

impl SourcePart {
    /// The part's name in a log line and a dump: its window's first and last
    /// line. Padded, so a file's parts sort in source order by name.
    pub fn label(&self) -> String {
        format!("{:06}-{:06}", self.first_line, self.last_line)
    }

    /// The window.
    pub fn window(&self) -> Lines {
        (self.first_line, self.last_line)
    }

    /// The lines the part shows.
    pub fn shown(&self) -> ShownLines {
        let mut shown = ShownLines {
            window: self.window(),
            runs: self.shown_runs.clone(),
            markers: false,
        };
        shown.markers = (shown.runs.len() - 1) * 10 < shown.line_count();
        shown
    }
}

/// One item of a list: its lines, the list it holds when it can be opened,
/// and whether it can state nothing the model is asked about.
#[derive(Debug)]
struct Item {
    first: u32,
    last: u32,
    list: Option<List>,
    /// Every sibling on the item's lines is a declaration that runs nothing
    /// ([`Node::states_nothing`]).
    states_nothing: bool,
}

/// The list of an item that can be opened.
#[derive(Debug)]
struct List {
    /// The owner's lines before its first item and after its last.
    opening: Lines,
    closing: Lines,
    /// The owner's lines, each in one item.
    items: Vec<Item>,
}

/// A file read once for cutting: its lines, its lists, where its candidates
/// start, and what each of its identifiers names.
#[derive(Debug)]
pub struct SourceCut {
    /// The byte each line starts at, and the end of the text.
    line_starts: Vec<usize>,
    /// The module's items: the whole file.
    root: List,
    /// Each candidate as (the line its span starts on, where it starts, its
    /// position in the prompt's listing), in source order.
    candidates: Vec<(u32, u32, usize)>,
    /// Every identifier as (its line, the binding it names), in source order.
    identifiers: Vec<(u32, BindingKey)>,
    /// The statements that declare each binding, as lines. Import
    /// declarations are not among them: the import table says what they say.
    declarations: HashMap<BindingKey, Vec<Lines>>,
}

/// The parts `content` is asked in under `limits`, in source order
/// (carrick#2308). `None` when the file does not parse.
pub fn source_parts(
    path: &Path,
    content: &str,
    listed: &[ListedCandidate],
    limits: &Limits,
) -> Option<Vec<SourcePart>> {
    Some(SourceCut::of(path, content, listed)?.parts(limits))
}

impl SourceCut {
    /// Read `content` for cutting. `None` when it does not parse, or holds no
    /// line.
    pub fn of(path: &Path, content: &str, listed: &[ListedCandidate]) -> Option<Self> {
        use swc_common::{FileName, SourceMap, sync::Lrc};
        use swc_ecma_parser::{Parser, StringInput, lexer::Lexer};

        let (syntax, is_typescript) = crate::parser::syntax_for_path(path);
        // A source map of its own, as the scan that raised the candidates
        // used: spans count from the start of this file.
        let source_map: Lrc<SourceMap> = Default::default();
        let source = source_map.new_source_file(
            Lrc::new(FileName::Real(path.to_path_buf())),
            content.to_string(),
        );
        let lexer = Lexer::new(
            syntax,
            Default::default(),
            StringInput::from(&*source),
            None,
        );
        let mut module = Parser::new_from(lexer).parse_module().ok()?;
        // The resolver gives every identifier the scope of the binding it
        // names, which is what a declaration is found by (`binding_scope`).
        GLOBALS.set(&Globals::new(), || {
            let unresolved = Mark::new();
            let top_level = Mark::new();
            module.visit_mut_with(&mut resolver(unresolved, top_level, is_typescript));
        });

        // Lines as the prompt numbers them: `str::lines` over the same text.
        let text: &str = &source.src;
        let line_count = text.lines().count();
        if line_count == 0 {
            return None;
        }
        let mut line_starts: Vec<usize> = Vec::with_capacity(line_count + 1);
        line_starts.push(0);
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(at, _)| at + 1)
                .take(line_count - 1),
        );
        line_starts.push(text.len());
        let reader = LineReader {
            line_starts: &line_starts,
            base: source.start_pos.0,
        };

        let whole = (1, line_count as u32);
        let items: Vec<Node<'_>> = module.body.iter().map(Node::ModuleItem).collect();
        let root = List {
            opening: (1, 0),
            closing: (whole.1 + 1, whole.1),
            items: reader.items(whole, &items),
        };

        let mut candidates: Vec<(u32, u32, usize)> = listed
            .iter()
            .enumerate()
            .map(|(position, candidate)| {
                let offset = candidate.span_start.saturating_sub(SWC_SPAN_BASE) as usize;
                (reader.line_at(offset), candidate.span_start, position)
            })
            .collect();
        candidates.sort_unstable();

        let mut names = Names {
            reader: &reader,
            identifiers: Vec::new(),
            declarations: HashMap::new(),
        };
        module.visit_with(&mut names);
        let Names {
            mut identifiers,
            declarations,
            ..
        } = names;
        // Stable: identifiers of one line keep the order they are written in.
        identifiers.sort_by_key(|(line, _)| *line);

        Some(Self {
            line_starts,
            root,
            candidates,
            identifiers,
            declarations,
        })
    }

    /// How many lines the file holds.
    pub fn line_count(&self) -> u32 {
        (self.line_starts.len() - 1) as u32
    }

    /// The bytes of the lines `first..=last`.
    fn bytes(&self, (first, last): Lines) -> usize {
        if first > last {
            return 0;
        }
        self.line_starts[last as usize] - self.line_starts[first as usize - 1]
    }

    /// The candidates whose span starts on a line of `first..=last`.
    fn candidates_in(&self, (first, last): Lines) -> &[(u32, u32, usize)] {
        let from = self
            .candidates
            .partition_point(|(line, _, _)| *line < first);
        let to = self
            .candidates
            .partition_point(|(line, _, _)| *line <= last);
        &self.candidates[from..to]
    }

    /// How many groups of candidates start on a line of `lines`: candidates
    /// that share a start are one group, and are never separated.
    fn groups_in(&self, lines: Lines) -> usize {
        let mut starts: Vec<u32> = self
            .candidates_in(lines)
            .iter()
            .map(|(_, start, _)| *start)
            .collect();
        starts.dedup();
        starts.len()
    }

    /// The identifiers on a line of `first..=last`.
    fn identifiers_in(&self, (first, last): Lines) -> &[(u32, BindingKey)] {
        let from = self.identifiers.partition_point(|(line, _)| *line < first);
        let to = self.identifiers.partition_point(|(line, _)| *line <= last);
        &self.identifiers[from..to]
    }

    /// The names written as identifiers on the lines `shown` holds. A
    /// property name (`x.name`, `{ name: 1 }`) is not an identifier.
    pub fn names_on(&self, shown: &ShownLines) -> HashSet<&str> {
        shown
            .runs
            .iter()
            .flat_map(|run| self.identifiers_in(*run))
            .map(|(_, (name, _))| name.as_str())
            .collect()
    }

    /// The file's parts under `limits`, in source order.
    ///
    /// In source order, the next item joins the window while the window stays
    /// at or under `window_bytes` and holds at most `candidates` candidates;
    /// otherwise the window is closed and another started. An item that alone
    /// is over a limit is opened and its own list packed by the same rule, to
    /// any depth, into the window that is open: a declaration above a large
    /// construct rides with the construct's first items and is not asked
    /// about alone. An item over a limit with no list is a window of its own.
    pub fn parts(&self, limits: &Limits) -> Vec<SourcePart> {
        let mut packing = Packing {
            limits,
            open: Vec::new(),
            bytes: 0,
            candidates: 0,
            closed: Vec::new(),
        };
        self.pack(&self.root, &mut Vec::new(), &mut packing);
        packing.close();
        packing
            .closed
            .into_iter()
            .map(|runs| self.part(runs, limits))
            .collect()
    }

    fn pack(&self, list: &List, path: &mut Vec<usize>, packing: &mut Packing<'_>) {
        let limits = packing.limits;
        for (index, item) in list.items.iter().enumerate() {
            let lines = (item.first, item.last);
            let bytes = self.bytes(lines);
            let candidates = self.candidates_in(lines).len();
            if bytes > limits.window_bytes || candidates > limits.candidates {
                match &item.list {
                    Some(inner) => {
                        path.push(index);
                        self.pack(inner, path, packing);
                        path.pop();
                    }
                    None => {
                        packing.close();
                        packing.add(path, index);
                        packing.close();
                    }
                }
                continue;
            }
            if packing.bytes + bytes > limits.window_bytes
                || packing.candidates + candidates > limits.candidates
            {
                packing.close();
            }
            packing.add(path, index);
            packing.bytes += bytes;
            packing.candidates += candidates;
        }
    }

    /// The item a path and a place in its list name.
    fn item_at(&self, path: &[usize], index: usize) -> &Item {
        let mut list = &self.root;
        for opened in path {
            list = list.items[*opened]
                .list
                .as_ref()
                .expect("a path names items that were opened");
        }
        &list.items[index]
    }

    /// The part that holds `runs`, which are consecutive in the source.
    fn part(&self, runs: Vec<Run>, limits: &Limits) -> SourcePart {
        let (first_path, first_items) = runs.first().expect("a part holds a run");
        let (last_path, last_items) = runs.last().expect("a part holds a run");
        let window = (
            self.item_at(first_path, first_items.start).first,
            self.item_at(last_path, last_items.end - 1).last,
        );
        // Every construct opened on the way to one of the runs, unless the
        // window holds all of it. Outermost first, each once.
        let mut enclosing: Vec<Enclosing> = Vec::new();
        for (path, _) in &runs {
            let mut list = &self.root;
            for opened in path {
                let item = &list.items[*opened];
                let inner = item
                    .list
                    .as_ref()
                    .expect("a path names items that were opened");
                let construct = Enclosing {
                    opening: inner.opening,
                    closing: inner.closing,
                };
                let held = item.first >= window.0 && item.last <= window.1;
                if !held && !enclosing.contains(&construct) {
                    enclosing.push(construct);
                }
                list = inner;
            }
        }
        let mut candidates: Vec<usize> = self
            .candidates_in(window)
            .iter()
            .map(|(_, _, position)| *position)
            .collect();
        candidates.sort_unstable();
        let context = self.context_of(window, limits.context_bytes);
        let shown_runs = self.shown_runs(window, &enclosing, &context);
        SourcePart {
            first_line: window.0,
            last_line: window.1,
            candidates,
            context,
            enclosing,
            runs,
            shown_runs,
        }
    }

    /// The runs of lines a part shows: its window, the opening and closing
    /// lines of the constructs it was cut out of and the declarations it
    /// uses, in source order. Two runs at most [`SHOWN_GAP_FILLED`] lines
    /// apart are one when those lines hold at most [`SHOWN_GAP_MAX_BYTES`].
    fn shown_runs(&self, window: Lines, enclosing: &[Enclosing], context: &[Lines]) -> Vec<Lines> {
        let mut runs: Vec<Lines> = vec![window];
        for construct in enclosing {
            runs.extend([construct.opening, construct.closing]);
        }
        runs.extend(context.iter().copied());
        runs.retain(|(first, last)| first <= last);
        runs.sort_unstable();
        let mut merged: Vec<Lines> = Vec::with_capacity(runs.len());
        for (first, last) in runs {
            match merged.last_mut() {
                Some(held)
                    if first <= held.1 + 1
                        || (first <= held.1 + 1 + SHOWN_GAP_FILLED
                            && self.bytes((held.1 + 1, first - 1)) <= SHOWN_GAP_MAX_BYTES) =>
                {
                    held.1 = held.1.max(last)
                }
                _ => merged.push((first, last)),
            }
        }
        merged
    }

    /// The declarations the lines of `window` use: the statements that
    /// declare a binding an identifier in the window names, then the ones
    /// those use, breadth first, until `budget` bytes are spent.
    ///
    /// Found by binding, never by name alone: a name the window declares
    /// again reaches no outer declaration. A statement that holds the window
    /// is not one of them, since its own lines are shown as the construct the
    /// window was cut out of.
    fn context_of(&self, window: Lines, mut budget: usize) -> Vec<Lines> {
        let mut chosen: BTreeSet<Lines> = BTreeSet::new();
        let mut seen: HashSet<&BindingKey> = HashSet::new();
        let mut queue: VecDeque<&BindingKey> = VecDeque::new();
        for (_, key) in self.identifiers_in(window) {
            if seen.insert(key) {
                queue.push_back(key);
            }
        }
        'spent: while let Some(key) = queue.pop_front() {
            let Some(declared) = self.declarations.get(key) else {
                continue;
            };
            for lines in declared {
                let overlaps_window = lines.0 <= window.1 && lines.1 >= window.0;
                if overlaps_window || chosen.contains(lines) {
                    continue;
                }
                let whole = self.bytes(*lines);
                let shown = if whole > CONTEXT_DECLARATION_MAX {
                    (lines.0, lines.0)
                } else {
                    *lines
                };
                if chosen.contains(&shown) {
                    continue;
                }
                let cost = self.bytes(shown);
                if cost > budget {
                    break 'spent;
                }
                budget -= cost;
                chosen.insert(shown);
                if shown != *lines {
                    continue;
                }
                for (_, used) in self.identifiers_in(shown) {
                    if seen.insert(used) {
                        queue.push_back(used);
                    }
                }
            }
        }
        chosen.into_iter().collect()
    }

    /// The two halves of a part whose own answer was cut, or `None` when it
    /// cannot be cut again.
    ///
    /// A part of several groups of candidates is cut between two of its items
    /// so that the front half holds half its groups. A part of one group is
    /// not cut: its candidates are never separated, so the verdict on it is
    /// the verdict on the file. A part with no candidate is cut at its middle
    /// item. A part that is one item is opened first, and its list is what is
    /// halved; one item with no list cannot be cut.
    pub fn halves(&self, part: &SourcePart, limits: &Limits) -> Option<(SourcePart, SourcePart)> {
        self.halve(part, limits, Halving::ByGroups)
    }

    /// The two halves of a part whose own prompt is too large to ask
    /// (carrick#2309, carrick#2310), or `None` when it cannot be cut again.
    ///
    /// A part is cut between two of its items so that the front half holds
    /// about half its bytes, whatever its candidates: a prompt's size is its
    /// lines', and candidates that share a start still share a line, so they
    /// are never separated. A part that is one item is opened first; one item
    /// with no list cannot be cut.
    pub fn halves_by_size(
        &self,
        part: &SourcePart,
        limits: &Limits,
    ) -> Option<(SourcePart, SourcePart)> {
        self.halve(part, limits, Halving::BySize)
    }

    /// Whether every item of `part`'s window is a declaration that runs
    /// nothing: an import, a re-export, a type alias, an interface or an
    /// ambient declaration (carrick#2309). Such a window, holding no
    /// candidate, states nothing the model is asked about.
    pub fn states_nothing(&self, part: &SourcePart) -> bool {
        part.runs.iter().all(|(path, held)| {
            held.clone()
                .all(|index| self.item_at(path, index).states_nothing)
        })
    }

    fn halve(
        &self,
        part: &SourcePart,
        limits: &Limits,
        halving: Halving,
    ) -> Option<(SourcePart, SourcePart)> {
        let groups = self.groups_in(part.window());
        if groups == 1 && halving == Halving::ByGroups {
            return None;
        }
        // The part's items, each with the list it sits in.
        let mut items: Vec<(Vec<usize>, usize)> = part
            .runs
            .iter()
            .flat_map(|(path, held)| held.clone().map(move |index| (path.clone(), index)))
            .collect();
        while let [(path, index)] = items.as_slice() {
            let inner = self.item_at(path, *index).list.as_ref()?;
            let mut opened = path.clone();
            opened.push(*index);
            items = (0..inner.items.len())
                .map(|index| (opened.clone(), index))
                .collect();
        }
        let middle = if halving == Halving::BySize {
            // The first boundary with half the bytes in front of it, and at
            // least one item behind it.
            let sizes: Vec<usize> = items
                .iter()
                .map(|(path, index)| {
                    let item = self.item_at(path, *index);
                    self.bytes((item.first, item.last))
                })
                .collect();
            let total: usize = sizes.iter().sum();
            let mut held = 0;
            let mut boundary = items.len() - 1;
            for (place, size) in sizes[..items.len() - 1].iter().enumerate() {
                held += size;
                if 2 * held >= total {
                    boundary = place + 1;
                    break;
                }
            }
            boundary
        } else if groups == 0 {
            items.len() / 2
        } else {
            // The first boundary with half the groups in front of it, and at
            // least one item behind it.
            let mut held = 0;
            let mut boundary = items.len() - 1;
            for (place, (path, index)) in items[..items.len() - 1].iter().enumerate() {
                let item = self.item_at(path, *index);
                held += self.groups_in((item.first, item.last));
                if held >= groups / 2 {
                    boundary = place + 1;
                    break;
                }
            }
            boundary
        };
        let runs_of = |items: &[(Vec<usize>, usize)]| -> Vec<Run> {
            let mut runs: Vec<Run> = Vec::new();
            for (path, index) in items {
                match runs.last_mut() {
                    Some((held, range)) if held == path && range.end == *index => range.end += 1,
                    _ => runs.push((path.clone(), *index..*index + 1)),
                }
            }
            runs
        };
        Some((
            self.part(runs_of(&items[..middle]), limits),
            self.part(runs_of(&items[middle..]), limits),
        ))
    }
}

/// A run of consecutive items of one list: the list, as the item opened at
/// each depth on the way to it, and which of its items.
type Run = (Vec<usize>, Range<usize>);

/// What a part is halved for: an answer that was cut, which is halved by its
/// candidates' groups, or a prompt too large to ask, which is halved by its
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halving {
    ByGroups,
    BySize,
}

/// The window being filled while a file is packed, and the ones filled.
struct Packing<'a> {
    limits: &'a Limits,
    open: Vec<Run>,
    bytes: usize,
    candidates: usize,
    closed: Vec<Vec<Run>>,
}

impl Packing<'_> {
    fn close(&mut self) {
        if !self.open.is_empty() {
            self.closed.push(std::mem::take(&mut self.open));
        }
        (self.bytes, self.candidates) = (0, 0);
    }

    fn add(&mut self, path: &[usize], index: usize) {
        match self.open.last_mut() {
            Some((held, range)) if held == path && range.end == index => range.end += 1,
            _ => self.open.push((path.to_vec(), index..index + 1)),
        }
    }
}

/// Where a position of the parsed text sits among its lines.
struct LineReader<'a> {
    line_starts: &'a [usize],
    /// The position the text starts at.
    base: u32,
}

impl LineReader<'_> {
    fn line_count(&self) -> u32 {
        (self.line_starts.len() - 1) as u32
    }

    /// The line the byte at `offset` is on.
    fn line_at(&self, offset: usize) -> u32 {
        let lines = &self.line_starts[..self.line_starts.len() - 1];
        (lines.partition_point(|start| *start <= offset) as u32).clamp(1, self.line_count())
    }

    fn line_of(&self, position: BytePos) -> u32 {
        self.line_at(position.0.saturating_sub(self.base) as usize)
    }

    /// The lines a span starts and ends on, or `None` for a span that names
    /// no place in the text.
    fn lines_of(&self, span: Span) -> Option<Lines> {
        if span.lo.0 < self.base || span.hi <= span.lo {
            return None;
        }
        Some((self.line_of(span.lo), self.line_of(BytePos(span.hi.0 - 1))))
    }

    /// The items `nodes` make of the lines `within`: each from the line
    /// after the one before it ends to the line it ends on, the first from
    /// the first line of `within` and the last to its last. Siblings that end
    /// on one line are one item, opened as the largest of them is.
    fn items(&self, within: Lines, nodes: &[Node<'_>]) -> Vec<Item> {
        // (the line the item ends on, its largest sibling and that one's
        // size, whether every sibling on it states nothing)
        let mut ends: Vec<(u32, Node<'_>, u32, bool)> = Vec::new();
        for node in nodes {
            let span = node.span();
            let Some((_, last)) = self.lines_of(span) else {
                continue;
            };
            let last = last.clamp(within.0, within.1);
            let size = span.hi.0 - span.lo.0;
            match ends.last_mut() {
                Some(held) if held.0 >= last => {
                    if size > held.2 {
                        (held.1, held.2) = (*node, size);
                    }
                    held.3 &= node.states_nothing();
                }
                _ => ends.push((last, *node, size, node.states_nothing())),
            }
        }
        let count = ends.len();
        let mut items = Vec::with_capacity(count);
        let mut first = within.0;
        for (index, (end, node, _, states_nothing)) in ends.into_iter().enumerate() {
            let last = if index + 1 == count { within.1 } else { end };
            let lines = (first, last);
            items.push(Item {
                first,
                last,
                list: self.list(lines, node),
                states_nothing,
            });
            first = last + 1;
        }
        items
    }

    /// The list of the item `node` on the lines `within`, when it has one.
    fn list(&self, within: Lines, node: Node<'_>) -> Option<List> {
        let (owner, nodes) = node.list()?;
        let (owner_first, owner_last) = self.lines_of(owner)?;
        let (first_child, _) = self.lines_of(nodes.first()?.span())?;
        let (_, last_child) = self.lines_of(nodes.last()?.span())?;
        let items = self.items(within, &nodes);
        if items.is_empty() {
            return None;
        }
        let (node_first, _) = self.lines_of(node.span())?;
        // From where the item starts to the line before its first child, and
        // always the line the list opens on.
        let opening_last = owner_first.max(first_child.saturating_sub(1)).min(within.1);
        let closing_first = owner_last.min(last_child + 1).max(within.0);
        Some(List {
            opening: (within.0.min(node_first), opening_last),
            closing: (closing_first, within.1),
            items,
        })
    }
}

/// A sibling of one of the lists a file can be cut along.
#[derive(Clone, Copy)]
enum Node<'a> {
    ModuleItem(&'a ModuleItem),
    Stmt(&'a Stmt),
    Member(&'a ClassMember),
    Expr(&'a Expr),
    Prop(&'a PropOrSpread),
    /// A block of a `try`: the block tried, the `catch` body, the `finally`.
    Block(&'a BlockStmt),
    /// A case of a `switch`.
    Case(&'a SwitchCase),
}

/// A list an item holds: the span of what owns it, and its siblings.
type Listed<'a> = Option<(Span, Vec<Node<'a>>)>;

impl<'a> Node<'a> {
    fn span(&self) -> Span {
        match self {
            Node::ModuleItem(item) => item.span(),
            Node::Stmt(stmt) => stmt.span(),
            Node::Member(member) => member.span(),
            Node::Expr(expr) => expr.span(),
            Node::Prop(prop) => prop.span(),
            Node::Block(block) => block.span,
            Node::Case(case) => case.span,
        }
    }

    /// The list of an item: for a declaration with one declarator, its
    /// initializer's; for a call, that of the largest of its arguments and a
    /// function it calls where it is written that is an array literal, an
    /// object literal or a function; for a function or a class, its body; for
    /// a statement that holds statements, those ([`stmt_list`]). `None` for
    /// an item with nothing to be cut along.
    fn list(&self) -> Listed<'a> {
        match self {
            Node::ModuleItem(ModuleItem::Stmt(stmt)) => stmt_list(stmt),
            Node::Stmt(stmt) => stmt_list(stmt),
            Node::ModuleItem(ModuleItem::ModuleDecl(decl)) => match decl {
                ModuleDecl::ExportDecl(export) => decl_list(&export.decl),
                ModuleDecl::ExportDefaultDecl(export) => match &export.decl {
                    DefaultDecl::Fn(function) => function_list(&function.function),
                    DefaultDecl::Class(class) => class_list(&class.class),
                    DefaultDecl::TsInterfaceDecl(_) => None,
                },
                ModuleDecl::ExportDefaultExpr(export) => expr_list(&export.expr),
                ModuleDecl::TsExportAssignment(export) => expr_list(&export.expr),
                _ => None,
            },
            Node::Member(member) => match member {
                ClassMember::Method(method) => function_list(&method.function),
                ClassMember::PrivateMethod(method) => function_list(&method.function),
                ClassMember::Constructor(constructor) => {
                    constructor.body.as_ref().and_then(body_list)
                }
                ClassMember::ClassProp(prop) => prop.value.as_deref().and_then(expr_list),
                ClassMember::PrivateProp(prop) => prop.value.as_deref().and_then(expr_list),
                ClassMember::StaticBlock(block) => block_list(&block.body),
                _ => None,
            },
            Node::Expr(expr) => expr_list(expr),
            Node::Prop(PropOrSpread::Prop(prop)) => match &**prop {
                Prop::KeyValue(pair) => expr_list(&pair.value),
                Prop::Method(method) => function_list(&method.function),
                Prop::Getter(getter) => function_list(&getter.function),
                Prop::Setter(setter) => function_list(&setter.function),
                _ => None,
            },
            Node::Prop(PropOrSpread::Spread(_)) => None,
            Node::Block(block) => block_list(block),
            Node::Case(case) => Some((case.span, case.cons.iter().map(Node::Stmt).collect())),
        }
    }

    /// Whether the item is a declaration that runs nothing, and so states
    /// nothing the model is asked about (carrick#2309): an import, a
    /// re-export, a type alias, an interface or an ambient declaration. Read
    /// off the kind of the item, never a name.
    fn states_nothing(&self) -> bool {
        match self {
            Node::ModuleItem(ModuleItem::ModuleDecl(decl)) => match decl {
                ModuleDecl::Import(_)
                | ModuleDecl::ExportAll(_)
                | ModuleDecl::TsImportEquals(_)
                | ModuleDecl::TsNamespaceExport(_) => true,
                ModuleDecl::ExportNamed(named) => named.src.is_some(),
                ModuleDecl::ExportDecl(export) => decl_states_nothing(&export.decl),
                ModuleDecl::ExportDefaultDecl(export) => {
                    matches!(export.decl, DefaultDecl::TsInterfaceDecl(_))
                }
                _ => false,
            },
            Node::ModuleItem(ModuleItem::Stmt(stmt)) => stmt_states_nothing(stmt),
            Node::Stmt(stmt) => stmt_states_nothing(stmt),
            Node::Member(_) | Node::Expr(_) | Node::Prop(_) | Node::Block(_) | Node::Case(_) => {
                false
            }
        }
    }
}

/// Whether `stmt` is an empty statement or a declaration that runs nothing.
fn stmt_states_nothing(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Decl(decl) => decl_states_nothing(decl),
        Stmt::Empty(_) => true,
        _ => false,
    }
}

/// Whether `decl` is a type alias, an interface or an ambient declaration.
fn decl_states_nothing(decl: &Decl) -> bool {
    match decl {
        Decl::TsInterface(_) | Decl::TsTypeAlias(_) => true,
        Decl::Var(var) => var.declare,
        Decl::Fn(function) => function.declare,
        Decl::Class(class) => class.declare,
        Decl::TsEnum(declared) => declared.declare,
        Decl::TsModule(namespace) => namespace.declare,
        Decl::Using(_) => false,
    }
}

fn block_list(block: &BlockStmt) -> Listed<'_> {
    Some((block.span, block.stmts.iter().map(Node::Stmt).collect()))
}

fn body_list(body: &FunctionBody) -> Listed<'_> {
    Some((body.span, body.stmts.iter().map(Node::Stmt).collect()))
}

fn function_list(function: &Function) -> Listed<'_> {
    function.body.as_ref().and_then(body_list)
}

fn class_list(class: &Class) -> Listed<'_> {
    Some((class.span, class.body.iter().map(Node::Member).collect()))
}

/// The list of a statement: a declaration's or an expression's as
/// [`Node::list`] says, and for a statement that holds statements, those
/// (carrick#2308). A statement with one body (a block, a labelled statement,
/// a loop) is cut along its body's statements. One with several (`if` and
/// its `else`, `try` and its `catch` and `finally`, `switch` and its cases)
/// is cut between them, and each is opened in turn.
fn stmt_list(stmt: &Stmt) -> Listed<'_> {
    match stmt {
        Stmt::Decl(decl) => decl_list(decl),
        Stmt::Expr(statement) => expr_list(&statement.expr),
        Stmt::Return(statement) => statement.arg.as_deref().and_then(expr_list),
        Stmt::Block(block) => block_list(block),
        Stmt::Labeled(statement) => stmt_list(&statement.body),
        Stmt::For(statement) => stmt_list(&statement.body),
        Stmt::ForIn(statement) => stmt_list(&statement.body),
        Stmt::ForOf(statement) => stmt_list(&statement.body),
        Stmt::While(statement) => stmt_list(&statement.body),
        Stmt::DoWhile(statement) => stmt_list(&statement.body),
        Stmt::If(statement) => Some((
            statement.span,
            std::iter::once(&*statement.cons)
                .chain(statement.alt.as_deref())
                .map(Node::Stmt)
                .collect(),
        )),
        Stmt::Try(statement) => Some((
            statement.span,
            std::iter::once(&statement.block)
                .chain(statement.handler.as_ref().map(|handler| &handler.body))
                .chain(statement.finalizer.as_ref())
                .map(Node::Block)
                .collect(),
        )),
        Stmt::Switch(statement) => Some((
            statement.span,
            statement.cases.iter().map(Node::Case).collect(),
        )),
        _ => None,
    }
}

fn decl_list(decl: &Decl) -> Listed<'_> {
    match decl {
        Decl::Var(var) => match var.decls.as_slice() {
            [declarator] => declarator.init.as_deref().and_then(expr_list),
            _ => None,
        },
        Decl::Fn(function) => function_list(&function.function),
        Decl::Class(class) => class_list(&class.class),
        Decl::TsModule(namespace) => namespace.body.as_ref().and_then(namespace_list),
        _ => None,
    }
}

fn namespace_list(body: &TsNamespaceBody) -> Listed<'_> {
    match body {
        TsNamespaceBody::TsModuleBlock(block) => Some((
            block.span,
            block.body.iter().map(Node::ModuleItem).collect(),
        )),
        TsNamespaceBody::TsNamespaceDecl(inner) => namespace_list(&inner.body),
    }
}

/// An expression with what only wraps it taken off: a type assertion, a
/// `satisfies`, parentheses, an `await`.
fn unwrapped(mut expr: &Expr) -> &Expr {
    loop {
        expr = match expr {
            Expr::Paren(inner) => &inner.expr,
            Expr::TsAs(inner) => &inner.expr,
            Expr::TsSatisfies(inner) => &inner.expr,
            Expr::TsConstAssertion(inner) => &inner.expr,
            Expr::TsTypeAssertion(inner) => &inner.expr,
            Expr::TsNonNull(inner) => &inner.expr,
            Expr::Await(inner) => &inner.arg,
            _ => return expr,
        };
    }
}

fn expr_list(expr: &Expr) -> Listed<'_> {
    match unwrapped(expr) {
        Expr::Array(array) => Some((
            array.span,
            array
                .elems
                .iter()
                .flatten()
                .map(|element| Node::Expr(&element.expr))
                .collect(),
        )),
        Expr::Object(object) => Some((object.span, object.props.iter().map(Node::Prop).collect())),
        Expr::Fn(function) => function_list(&function.function),
        Expr::Arrow(arrow) => match &*arrow.body {
            ArrowFunctionBody::FunctionBody(body) => body_list(body),
            ArrowFunctionBody::Expr(body) => expr_list(body),
        },
        Expr::Class(class) => class_list(&class.class),
        // A function called where it is written, `(function () { ... })()`,
        // is one of what the call can be cut along, beside its arguments.
        Expr::Call(call) => {
            let callee = match &call.callee {
                Callee::Expr(callee) => Some(unwrapped(callee))
                    .filter(|callee| matches!(callee, Expr::Fn(_) | Expr::Arrow(_))),
                _ => None,
            };
            largest_list(callee.into_iter().chain(arguments(&call.args)))
        }
        Expr::New(new) => largest_list(new.args.as_deref().into_iter().flat_map(arguments)),
        // `!function () { ... }()`: the operand is the item's.
        Expr::Unary(unary) => expr_list(&unary.arg),
        // `module.exports = [ ... ]`: the value assigned is the item's.
        Expr::Assign(assign) => expr_list(&assign.right),
        _ => None,
    }
}

fn arguments(arguments: &[ExprOrSpread]) -> impl DoubleEndedIterator<Item = &Expr> {
    arguments.iter().map(|argument| &*argument.expr)
}

/// The list of the largest of `exprs` that is an array literal, an object
/// literal or a function.
fn largest_list<'a>(exprs: impl DoubleEndedIterator<Item = &'a Expr>) -> Listed<'a> {
    exprs
        .map(unwrapped)
        .filter(|argument| {
            matches!(
                argument,
                Expr::Array(_) | Expr::Object(_) | Expr::Fn(_) | Expr::Arrow(_) | Expr::Class(_)
            )
        })
        // The first of two the same size, so the choice is the source's.
        .rev()
        .max_by_key(|argument| {
            let span = argument.span();
            span.hi.0 - span.lo.0
        })
        .and_then(expr_list)
}

/// Every identifier of a module and the statements that declare bindings.
struct Names<'a> {
    reader: &'a LineReader<'a>,
    identifiers: Vec<(u32, BindingKey)>,
    declarations: HashMap<BindingKey, Vec<Lines>>,
}

impl Names<'_> {
    fn declare(&mut self, key: BindingKey, statement: Span) {
        let Some(lines) = self.reader.lines_of(statement) else {
            return;
        };
        let declared = self.declarations.entry(key).or_default();
        if !declared.contains(&lines) {
            declared.push(lines);
        }
    }

    /// Record the bindings `decl` declares as declared by `statement`.
    fn declare_all(&mut self, decl: &Decl, statement: Span) {
        match decl {
            Decl::Var(var) => {
                for declarator in &var.decls {
                    self.declare_pattern(&declarator.name, statement);
                }
            }
            Decl::Using(using) => {
                for declarator in &using.decls {
                    self.declare_pattern(&declarator.name, statement);
                }
            }
            Decl::Fn(function) => self.declare(ident_key(&function.ident), statement),
            Decl::Class(class) => self.declare(ident_key(&class.ident), statement),
            Decl::TsInterface(interface) => self.declare(ident_key(&interface.id), statement),
            Decl::TsTypeAlias(alias) => self.declare(ident_key(&alias.id), statement),
            Decl::TsEnum(declared) => self.declare(ident_key(&declared.id), statement),
            Decl::TsModule(namespace) => {
                if let TsModuleName::Ident(ident) = &namespace.id {
                    self.declare(ident_key(ident), statement);
                }
            }
        }
    }

    fn declare_pattern(&mut self, pattern: &Pat, statement: Span) {
        let mut bound = PatternBindings::default();
        pattern.visit_with(&mut bound);
        for key in bound.keys {
            self.declare(key, statement);
        }
    }
}

impl Visit for Names<'_> {
    fn visit_ident(&mut self, ident: &Ident) {
        if ident.span.lo.0 >= self.reader.base {
            self.identifiers
                .push((self.reader.line_of(ident.span.lo), ident_key(ident)));
        }
    }

    fn visit_module_item(&mut self, item: &ModuleItem) {
        if let ModuleItem::ModuleDecl(decl) = item {
            match decl {
                ModuleDecl::ExportDecl(export) => self.declare_all(&export.decl, export.span),
                ModuleDecl::ExportDefaultDecl(export) => match &export.decl {
                    DefaultDecl::Fn(function) => {
                        if let Some(ident) = &function.ident {
                            self.declare(ident_key(ident), export.span);
                        }
                    }
                    DefaultDecl::Class(class) => {
                        if let Some(ident) = &class.ident {
                            self.declare(ident_key(ident), export.span);
                        }
                    }
                    DefaultDecl::TsInterfaceDecl(interface) => {
                        self.declare(ident_key(&interface.id), export.span);
                    }
                },
                _ => {}
            }
        }
        item.visit_children_with(self);
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        if let Stmt::Decl(decl) = stmt {
            self.declare_all(decl, decl.span());
        }
        stmt.visit_children_with(self);
    }
}

/// The bindings a pattern introduces. A default value is an expression and
/// binds nothing of the pattern's.
#[derive(Default)]
struct PatternBindings {
    keys: Vec<BindingKey>,
}

impl Visit for PatternBindings {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.keys.push(ident_key(&ident.id));
    }

    fn visit_expr(&mut self, _: &Expr) {}

    fn visit_ts_type(&mut self, _: &TsType) {}
}

#[cfg(test)]
mod tests {
    use super::shapes::{self, Shape};
    use super::*;

    /// What `ask_in_parts` cuts a file under.
    const LIMITS: Limits = Limits {
        window_bytes: 64_000,
        context_bytes: 16_000,
        candidates: 32,
    };

    /// How many requests or routes each generated file holds.
    const N: usize = 400;

    #[test]
    fn a_part_numbers_its_shown_lines_and_skips_one_number_a_gap() {
        let shown = ShownLines {
            window: (6, 8),
            runs: vec![(2, 2), (6, 8), (11, 12)],
            markers: true,
        };
        for (file, prompt) in [(2, 1), (6, 3), (8, 5), (11, 7), (12, 8)] {
            assert_eq!(shown.prompt_line(file), Some(prompt), "file line {file}");
        }
        assert_eq!(shown.prompt_line(3), None, "a line in a gap is not shown");
        assert_eq!(shown.prompt_line(13), None);
        for (prompt, file) in [(1, 2), (3, 6), (5, 8), (7, 11), (8, 12)] {
            assert_eq!(shown.file_line(prompt), Some(file), "prompt line {prompt}");
        }
        for prompt in [2, 6, 9, 0, -1] {
            assert_eq!(shown.file_line(prompt), None, "prompt line {prompt}");
        }
    }

    fn listed_at(starts: &[usize]) -> Vec<ListedCandidate> {
        starts
            .iter()
            .map(|start| {
                let start = *start as u32 + SWC_SPAN_BASE;
                ListedCandidate {
                    candidate_id: format!("span:{start}-{}", start + 8),
                    span_start: start,
                    span_end: start + 8,
                }
            })
            .collect()
    }

    fn parts_of(shape: &Shape, limits: &Limits) -> Vec<SourcePart> {
        source_parts(
            Path::new("src/generated.ts"),
            &shape.source,
            &listed_at(&shape.starts),
            limits,
        )
        .expect("a generated file parses")
    }

    /// The text of the lines `first..=last`, newlines included.
    fn text_of(source: &str, (first, last): Lines) -> String {
        source
            .split_inclusive('\n')
            .skip(first as usize - 1)
            .take((last - first + 1) as usize)
            .collect()
    }

    fn line(source: &str, number: u32) -> &str {
        source.lines().nth(number as usize - 1).expect("a line")
    }

    #[test]
    fn the_windows_of_a_file_are_its_lines_once_each() {
        for (name, shape) in shapes::every_shape("once", N) {
            let parts = parts_of(&shape, &LIMITS);
            assert!(parts.len() > 1, "{name}: {} part(s)", parts.len());
            let joined: String = parts
                .iter()
                .map(|part| text_of(&shape.source, part.window()))
                .collect();
            assert_eq!(joined, shape.source, "{name}");
            assert_eq!(parts[0].first_line, 1, "{name}");
            for pair in parts.windows(2) {
                assert_eq!(pair[1].first_line, pair[0].last_line + 1, "{name}");
            }
        }
    }

    #[test]
    fn no_window_is_over_its_limits() {
        for (name, shape) in shapes::every_shape("limits", N) {
            for part in parts_of(&shape, &LIMITS) {
                let bytes = text_of(&shape.source, part.window()).len();
                assert!(bytes <= LIMITS.window_bytes, "{name}: {bytes} bytes");
                assert!(part.candidates.len() <= LIMITS.candidates, "{name}");
            }
        }
        // Under a limit its one import line passes, the flat module's parts
        // still keep to the limit, all but the line itself: an item with no
        // list is a window of its own.
        let tight = Limits {
            window_bytes: 2_000,
            ..LIMITS
        };
        let shape = shapes::flat_module("tight", N);
        let over: Vec<Lines> = parts_of(&shape, &tight)
            .iter()
            .filter(|part| text_of(&shape.source, part.window()).len() > tight.window_bytes)
            .map(SourcePart::window)
            .collect();
        assert_eq!(over, [(1, 1)], "only the import line is over");
    }

    #[test]
    fn every_candidate_is_in_one_window() {
        for (name, shape) in shapes::every_shape("held", N) {
            let listed = listed_at(&shape.starts);
            let mut held = vec![0; listed.len()];
            for part in parts_of(&shape, &LIMITS) {
                for position in &part.candidates {
                    held[*position] += 1;
                    let offset = (listed[*position].span_start - SWC_SPAN_BASE) as usize;
                    let on = shape.source[..offset].matches('\n').count() as u32 + 1;
                    assert!(
                        (part.first_line..=part.last_line).contains(&on),
                        "{name}: candidate {position} starts on line {on}, outside {:?}",
                        part.window()
                    );
                }
            }
            assert!(held.iter().all(|times| *times == 1), "{name}: {held:?}");
        }
    }

    /// Every window but the last ends on a line that closes one sibling, and
    /// every window is cut out of the one construct that holds the list.
    fn assert_cut_between(shape: &Shape, limits: &Limits, closes_a_sibling: &str) {
        assert_cut_between_at(shape, limits, closes_a_sibling, 1);
    }

    /// [`assert_cut_between`] for a list `depth` constructs down: a statement
    /// that holds blocks (`if`, `try`, `switch`) is one construct and the
    /// block another.
    fn assert_cut_between_at(shape: &Shape, limits: &Limits, closes_a_sibling: &str, depth: usize) {
        let parts = parts_of(shape, limits);
        assert!(parts.len() > 2, "{} part(s)", parts.len());
        for part in &parts[..parts.len() - 1] {
            assert_eq!(line(&shape.source, part.last_line), closes_a_sibling);
        }
        for part in &parts {
            assert_eq!(part.enclosing.len(), depth, "{:?}", part.window());
        }
        // The lines the construct opens and closes on, the same for every
        // window cut out of it. The first window holds the opening lines,
        // under the declaration above the construct, which is not asked about
        // alone; the last window holds the closing ones.
        let Enclosing { opening, closing } = parts[1].enclosing[0].clone();
        assert_eq!(parts[0].first_line, 1);
        assert!(opening.0 > 1 && opening.1 < parts[1].first_line);
        assert_eq!(closing.1, shape.source.lines().count() as u32);
        assert!(
            parts
                .iter()
                .all(|part| part.enclosing[0] == parts[1].enclosing[0])
        );
    }

    #[test]
    fn a_literal_that_is_most_of_a_file_is_cut_between_its_elements() {
        let shape = shapes::route_array("literal", N);
        assert_cut_between(&shape, &LIMITS, "  },");
        // By size alone: no candidate, and a window that holds a few elements.
        let unlisted = Shape {
            source: shape.source,
            starts: Vec::new(),
        };
        let by_size = Limits {
            window_bytes: 4_000,
            ..LIMITS
        };
        assert_cut_between(&unlisted, &by_size, "  },");
    }

    #[test]
    fn a_function_body_that_is_most_of_a_file_is_cut_between_its_statements() {
        assert_cut_between(&shapes::registering_function("body", N), &LIMITS, "  });");
    }

    #[test]
    fn a_class_that_is_most_of_a_file_is_cut_between_its_members() {
        assert_cut_between(&shapes::request_class("class", N), &LIMITS, "  }");
    }

    #[test]
    fn a_function_called_where_it_is_written_is_cut_between_its_statements() {
        assert_cut_between(&shapes::called_function("called", N), &LIMITS, "  });");
    }

    #[test]
    fn a_try_block_that_is_most_of_a_file_is_cut_between_its_statements() {
        assert_cut_between_at(&shapes::try_block("tried", N), &LIMITS, "  });", 2);
    }

    /// Every statement the language defines to hold statements, and every
    /// way of calling a function where it is written, is cut between the
    /// statements it holds, each candidate in one window.
    #[test]
    fn every_statement_that_holds_statements_is_cut_between_them() {
        // (opening lines, closing lines, how many constructs down the list
        // is). An `if` with no `else` opens and closes on its block's lines,
        // so the two are one construct.
        let constructs: [(&str, &str, usize); 15] = [
            ("{", "}", 1),
            ("outer: {", "}", 1),
            ("for (let i = 0; i < 1; i++) {", "}", 1),
            ("for (const key in table) {", "}", 1),
            ("for (const item of items) {", "}", 1),
            ("while (running()) {", "}", 1),
            ("do {", "} while (running());", 1),
            ("if (ready) {", "}", 1),
            ("if (ready) {\n  start();\n} else {", "}", 2),
            ("try {\n  start();\n} catch (error) {", "}", 2),
            ("try {\n  start();\n} finally {", "}", 2),
            ("switch (mode) {\n  case \"serve\":", "    break;\n}", 2),
            ("(() => {", "})();", 1),
            ("!function () {", "}();", 1),
            ("await (async () => {", "})();", 1),
        ];
        let n = 100;
        for (opening, closing, depth) in constructs {
            let shape = shapes::statements_inside("construct", n, opening, closing);
            assert_cut_between_at(&shape, &LIMITS, "  });", depth);
            let mut held = vec![0; n];
            for part in parts_of(&shape, &LIMITS) {
                assert!(part.candidates.len() <= LIMITS.candidates, "{opening}");
                for position in part.candidates {
                    held[position] += 1;
                }
            }
            assert!(held.iter().all(|times| *times == 1), "{opening}: {held:?}");
        }
    }

    const USES: &str = "\
import { remote } from \"./remote\";
const BASE = \"/api\";
const unused = 1;
function shared(req: any, res: any) {
  res.json({ base: BASE });
}
export function register(app: any) {
  const prefix = BASE + \"/v1\";
  app.get(prefix + \"/a\", shared);
  app.get(\"/b\", (req: any, res: any) => {
    const shared = remote(req);
    res.json(shared);
  });
  app.get(prefix + \"/c\", shared);
}
";

    /// One candidate a window, so `register` is opened and its statements are
    /// windows.
    fn uses_parts() -> Vec<SourcePart> {
        let starts: Vec<usize> = USES.match_indices("app.get(").map(|(at, _)| at).collect();
        assert_eq!(starts.len(), 3);
        let one_a_window = Limits {
            candidates: 1,
            ..LIMITS
        };
        source_parts(
            Path::new("src/uses.ts"),
            USES,
            &listed_at(&starts),
            &one_a_window,
        )
        .expect("the file parses")
    }

    #[test]
    fn a_window_carries_the_declarations_it_uses() {
        let parts = uses_parts();
        let windows: Vec<Lines> = parts.iter().map(SourcePart::window).collect();
        assert_eq!(windows, [(1, 9), (10, 13), (14, 15)]);
        let register = Enclosing {
            opening: (7, 7),
            closing: (15, 15),
        };

        // The last registration names a local declared earlier in the same
        // body and a handler declared at module level; both use the module's
        // constant. Each is shown under its own line numbers, and the import
        // is not: the import table says what it says.
        let last = &parts[2];
        assert_eq!(last.context, [(2, 2), (4, 6), (8, 8)]);
        assert_eq!(last.enclosing, std::slice::from_ref(&register));
        assert!(!last.context.contains(&(3, 3)), "an unused declaration");

        // The inline handler declares `shared` again. Its window names that
        // binding, and the function of the same name is not pulled in.
        let shadowing = &parts[1];
        assert!(shadowing.context.is_empty(), "{:?}", shadowing.context);

        // The first window runs from the module's first line into the
        // function. It holds every declaration it uses, and it is cut out of
        // the function all the same.
        let first = &parts[0];
        assert!(first.context.is_empty(), "{:?}", first.context);
        assert_eq!(first.enclosing, [register]);
    }

    #[test]
    fn a_declaration_over_the_cap_is_shown_by_its_first_line() {
        let mut source = String::from("const table = [\n");
        for i in 0..400 {
            source.push_str(&format!("  \"/declared/in/a/long/table/{i}\",\n"));
        }
        source.push_str("];\n");
        // More than a window of other declarations, so the last window holds
        // no line of the table.
        for i in 0..200 {
            source.push_str(&format!("const filler{i} = {i};\n"));
        }
        source.push_str("export const first = table[0];\n");
        let last = source.lines().count() as u32;
        let tight = Limits {
            window_bytes: 3_000,
            ..LIMITS
        };
        let parts =
            source_parts(Path::new("src/table.ts"), &source, &[], &tight).expect("it parses");
        let window = parts.last().expect("a part");
        assert_eq!(window.last_line, last);
        assert!(window.first_line > 402, "{:?}", window.window());
        assert_eq!(window.context, [(1, 1)]);
    }

    #[test]
    fn the_same_file_gives_the_same_parts() {
        for (name, shape) in shapes::every_shape("same", N) {
            let listed = listed_at(&shape.starts);
            let path = Path::new("src/generated.ts");
            let first = source_parts(path, &shape.source, &listed, &LIMITS).unwrap();
            let again = source_parts(path, &shape.source, &listed, &LIMITS).unwrap();
            assert_eq!(first, again, "{name}");

            // The same candidates, listed last first.
            let reversed: Vec<ListedCandidate> = listed.iter().rev().cloned().collect();
            let other = source_parts(path, &shape.source, &reversed, &LIMITS).unwrap();
            let ids = |listing: &[ListedCandidate], part: &SourcePart| -> BTreeSet<String> {
                part.candidates
                    .iter()
                    .map(|position| listing[*position].candidate_id.clone())
                    .collect()
            };
            assert_eq!(first.len(), other.len(), "{name}");
            for (a, b) in first.iter().zip(&other) {
                assert_eq!(a.window(), b.window(), "{name}");
                assert_eq!((&a.context, &a.enclosing), (&b.context, &b.enclosing));
                assert_eq!(ids(&listed, a), ids(&reversed, b), "{name}");
                assert!(b.candidates.windows(2).all(|pair| pair[0] < pair[1]));
            }
        }
    }

    /// carrick#2309: a window states nothing exactly when every item it holds
    /// is an import, a re-export, a type alias, an interface or an ambient
    /// declaration, read off the item's kind.
    #[test]
    fn a_window_of_declarations_that_run_nothing_states_nothing() {
        let cut_of = |source: &str| {
            SourceCut::of(Path::new("src/generated.ts"), source, &[]).expect("it parses")
        };
        let window = |cut: &SourceCut| -> SourcePart {
            let [whole] = cut.parts(&LIMITS).try_into().expect("one part");
            whole
        };
        for nothing in [
            "import { a } from \"./a\";\n",
            "import * as b from \"./b\";\nexport * from \"./c\";\nexport { d } from \"./d\";\n",
            "export type T = { value: string };\ninterface I { value: number }\n",
            "export interface E { value: string }\nexport default interface F {}\n",
            "declare const client: any;\ndeclare function make(): any;\ndeclare class K {}\n",
            "declare enum Shade { Dark }\ndeclare namespace Space { const x: number; }\n",
            "export declare const declared: string;\n",
            "import x = require(\"./x\");\n",
        ] {
            let cut = cut_of(nothing);
            assert!(cut.states_nothing(&window(&cut)), "{nothing}");
        }
        for something in [
            "const table = makeTable();\n",
            "export const value = 1;\n",
            "export function f() {}\n",
            "enum Shade { Dark }\n",
            "namespace Space { export const x = 1; }\n",
            "export { local };\nconst local = 1;\n",
            "import { a } from \"./a\";\na();\n",
            "export default {};\n",
            "type T = string; call();\n",
        ] {
            let cut = cut_of(something);
            assert!(!cut.states_nothing(&window(&cut)), "{something}");
        }
    }

    /// carrick#2309: a short gap between a window and a declaration it uses
    /// is shown, and a gap of one long line is not: it could be the item the
    /// window was cut to leave out.
    #[test]
    fn a_long_line_between_a_window_and_its_context_is_not_shown() {
        let shown_with_gap = |gap: &str| {
            let source = format!(
                "declare const client: any;\n{gap}\nexport function f() {{\n  return client.get({{ url: \"/one\" }});\n}}\n"
            );
            let cut = SourceCut::of(Path::new("src/generated.ts"), &source, &[]).expect("parses");
            // The function's window, and the declaration it uses.
            assert_eq!(cut.context_of((3, 5), LIMITS.context_bytes), [(1, 1)]);
            cut.shown_runs((3, 5), &[], &[(1, 1)])
        };
        assert_eq!(shown_with_gap("export const short = 1;"), [(1, 5)]);
        let long = format!("export const long = \"{}\";", "x".repeat(1_000));
        assert_eq!(shown_with_gap(&long), [(1, 1), (3, 5)]);
    }

    /// carrick#2309: a part too large to ask is halved by its bytes, whatever
    /// its candidates, opening an item that is the whole part; one item with
    /// no list cannot be halved.
    #[test]
    fn a_part_is_halved_by_its_bytes_down_to_one_item() {
        // One group of candidates, which halving for a cut answer refuses.
        let mut source = String::from("declare const client: any;\n");
        source.push_str(&format!("export const big = \"{}\";\n", "x".repeat(900)));
        source.push_str("export function f() {\n");
        for i in 0..6 {
            source.push_str(&format!("  const v{i} = {i};\n"));
        }
        let start = source.len();
        source.push_str("  return client.get({ url: \"/one\" });\n}\n");
        let listed = listed_at(&[start + 9]);
        let cut = SourceCut::of(Path::new("src/generated.ts"), &source, &listed).expect("parses");
        let [whole] = cut.parts(&LIMITS).try_into().expect("one part");
        assert!(cut.halves(&whole, &LIMITS).is_none(), "one group");

        let (front, back) = cut.halves_by_size(&whole, &LIMITS).expect("three items");
        assert_eq!(
            front.window(),
            (1, 2),
            "the first half holds half the bytes"
        );
        assert_eq!(back.window(), (3, cut.line_count()));
        assert_eq!(back.candidates.len(), 1);
        // The function alone is opened and its statements halved.
        let (head, tail) = cut.halves_by_size(&back, &LIMITS).expect("its statements");
        assert_eq!(head.first_line, 3);
        assert_eq!(tail.last_line, cut.line_count());
        assert_eq!(tail.first_line, head.last_line + 1);

        // One line holding one item with nothing to cut along.
        let one = format!("export const big = \"{}\";\n", "x".repeat(900));
        let cut = SourceCut::of(Path::new("src/generated.ts"), &one, &[]).expect("parses");
        let [whole] = cut.parts(&LIMITS).try_into().expect("one part");
        assert!(cut.halves_by_size(&whole, &LIMITS).is_none());
    }

    #[test]
    fn a_part_is_halved_between_its_items_down_to_one_group() {
        let shape = shapes::registering_function("halved", 8);
        let listed = listed_at(&shape.starts);
        let cut = SourceCut::of(Path::new("src/generated.ts"), &shape.source, &listed)
            .expect("it parses");
        let [whole] = cut.parts(&LIMITS).try_into().expect("one part");
        assert_eq!(whole.window(), (1, cut.line_count()));

        // The whole file is two items, and all eight groups are in one. The
        // cut that separates them opens nothing; the next one opens the
        // function and halves its statements.
        let (head, function) = cut.halves(&whole, &LIMITS).expect("two items");
        assert_eq!((head.candidates.len(), function.candidates.len()), (0, 8));
        let (front, back) = cut.halves(&function, &LIMITS).expect("eight statements");
        assert_eq!((front.candidates.len(), back.candidates.len()), (4, 4));
        assert_eq!(back.first_line, front.last_line + 1);
        assert_eq!(
            (front.first_line, back.last_line),
            function.window(),
            "the halves are the part's lines"
        );
        assert_eq!(front.enclosing.len(), 1);

        let mut part = back;
        for groups in [2, 1] {
            part = cut.halves(&part, &LIMITS).expect("several groups").1;
            assert_eq!(part.candidates.len(), groups);
        }
        assert!(cut.halves(&part, &LIMITS).is_none(), "one group is not cut");

        // One item with no list is not cut.
        assert_eq!(head.window(), (1, 1));
        assert!(cut.halves(&head, &LIMITS).is_none());
    }

    #[test]
    fn a_part_with_no_candidate_is_halved_at_its_middle_item() {
        let shape = shapes::route_array("unlisted", 8);
        let cut =
            SourceCut::of(Path::new("src/generated.ts"), &shape.source, &[]).expect("it parses");
        let [whole] = cut.parts(&LIMITS).try_into().expect("one part");
        let (_, array) = cut.halves(&whole, &LIMITS).expect("two items");
        let (front, back) = cut.halves(&array, &LIMITS).expect("eight elements");
        let elements = |part: &SourcePart| {
            text_of(&shape.source, part.window())
                .matches("method:")
                .count()
        };
        assert_eq!((elements(&front), elements(&back)), (4, 4));
        assert_eq!(line(&shape.source, front.last_line), "  },");
    }

    /// The cloud strips the line numbers off a fence only while nearly every
    /// line carries one (carrick-cloud#2266).
    #[test]
    fn the_lines_that_carry_no_number_stay_under_one_in_ten() {
        let mut with_markers = 0;
        let mut parts: Vec<SourcePart> = uses_parts();
        for (_, shape) in shapes::every_shape("markers", N) {
            parts.extend(parts_of(&shape, &LIMITS));
        }
        for part in &parts {
            let shown = part.shown();
            assert!(shown.holds(part.first_line) && shown.holds(part.last_line));
            assert!(shown.runs.windows(2).all(|pair| pair[0].1 + 1 < pair[1].0));
            if shown.markers {
                with_markers += 1;
                assert!((shown.runs.len() - 1) * 10 < shown.line_count());
            }
        }
        assert!(with_markers > 0, "a large window names what it leaves out");
        // A window of two lines shown with seven lines of what it uses. The
        // one unused line between two of those is shown, since a line saying
        // it is missing would cost as much, and the one gap left is one line
        // without a number in nine: none is written.
        let small = uses_parts()[2].shown();
        assert_eq!(small.runs, [(2, 8), (14, 15)]);
        assert!(!small.markers);
    }

    /// A chain of registrations is one call written on one line: its links
    /// share a start, so they are one item, and a limit they pass does not
    /// separate them.
    #[test]
    fn candidates_that_share_a_start_stay_in_one_window() {
        let source = "\
declare const table: any;
table.get(\"/a\", () => {});
table.route(\"/chain\").get(() => {}).post(() => {}).put(() => {});
table.get(\"/b\", () => {});
";
        let chain = source.find("table.route").unwrap() as u32 + SWC_SPAN_BASE;
        let single = |path: &str| source.find(path).unwrap() as u32 + SWC_SPAN_BASE;
        let listed: Vec<ListedCandidate> = [
            (single("table.get(\"/a\""), 20),
            (chain, 60),
            (chain, 45),
            (chain, 30),
            (single("table.get(\"/b\""), 20),
        ]
        .into_iter()
        .map(|(start, length)| ListedCandidate {
            candidate_id: format!("span:{start}-{}", start + length),
            span_start: start,
            span_end: start + length,
        })
        .collect();
        let two = Limits {
            candidates: 2,
            ..LIMITS
        };
        let parts = source_parts(Path::new("src/table.ts"), source, &listed, &two).unwrap();
        let held: Vec<(Lines, &[usize])> = parts
            .iter()
            .map(|part| (part.window(), part.candidates.as_slice()))
            .collect();
        assert_eq!(
            held,
            [
                ((1, 2), &[0][..]),
                ((3, 3), &[1, 2, 3][..]),
                ((4, 4), &[4][..])
            ]
        );
        let cut = SourceCut::of(Path::new("src/table.ts"), source, &listed).unwrap();
        assert!(cut.halves(&parts[1], &two).is_none(), "one group");
    }

    #[test]
    fn a_file_that_does_not_parse_has_no_parts() {
        assert!(source_parts(Path::new("src/broken.ts"), "const = ;\n", &[], &LIMITS).is_none());
        assert!(source_parts(Path::new("src/empty.ts"), "", &[], &LIMITS).is_none());
    }

    #[test]
    fn a_part_is_named_by_its_window_and_names_sort_in_source_order() {
        let shape = shapes::request_class("named", N);
        let labels: Vec<String> = parts_of(&shape, &LIMITS)
            .iter()
            .map(SourcePart::label)
            .collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted);
        assert!(labels[0].starts_with("000001-"), "{}", labels[0]);
    }
}
