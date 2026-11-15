//! The name census (carrick#3504, build ticket B10-0): how each reader
//! came by the name of a message site, counted per service and written to
//! the run log as one `message names:` debug line.
//!
//! Two readers answer "what is this message site's name" today: the library
//! reader ([`crate::library_claims`]) and the site path
//! ([`crate::site_rows`]). This module is the instrument that says how often
//! each one reads a literal, a constant, a parameter its callers fill or
//! nothing, and whether the two agree where both state a row. It is log
//! only: no row, no blob field and no cache key reads it, so counting moves
//! nothing.

use std::collections::BTreeMap;
use std::path::Path;

use crate::library_claims::LibraryRow;
use crate::site_rows::SiteRow;

/// A word a census counts under.
pub trait Word: Copy + Ord + 'static {
    /// Every word, in the order the log line lists them: a word counted
    /// zero times is still listed.
    const ALL: &'static [Self];

    fn word(self) -> &'static str;
}

/// How often each word was counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally<K: Word>(BTreeMap<K, usize>);

impl<K: Word> Default for Tally<K> {
    fn default() -> Self {
        Tally(BTreeMap::new())
    }
}

impl<K: Word> Tally<K> {
    pub fn note(&mut self, word: K) {
        *self.0.entry(word).or_default() += 1;
    }

    pub fn count(&self, word: K) -> usize {
        self.0.get(&word).copied().unwrap_or_default()
    }

    /// `word=count` for every word, as the log prints it.
    pub fn line(&self) -> String {
        K::ALL
            .iter()
            .map(|word| format!("{}={}", word.word(), self.count(*word)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What the library reader read at a site's name slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameOutcome {
    /// The call writes the name itself.
    Literal,
    /// The name is read through a module-scope binding (a constant, an entry
    /// of a constant object, a builder).
    Linked,
    /// The name is a parameter's, read as the text a caller passes.
    Filled,
    /// The name is the one the maker that made the receiver was handed,
    /// read where the maker is written (carrick#3504 B10-3, Q-B3b).
    Made,
    /// Today's reading, kept: the text reader has no compiler rows for the
    /// slot (B10-3, Q-B3a).
    NoRows,
    /// Today's reading, kept: the slot calls an own builder the text reader
    /// holes (B10-3, until F4).
    R1Builder,
    /// A spread stands where the name is, so nothing is read.
    StopSpread,
    /// Anything else the reader cannot read as text.
    StopUnread,
    /// The text reader states no name, and the reader saves a lead
    /// (B10-3).
    Lead,
    /// A lead where today's reading stated a text: a row the text reader
    /// does not keep (B10-3).
    LeadTodayStated,
    /// A name the text reader reads where today's reading read none, on a
    /// label that does not say topic or job name: the reader states no row,
    /// and the site path's kind question decides (B10-3, Q-B3e).
    KindAsked,
    /// Today's reading, kept at a caller: a cap of the text reader (its
    /// step cap, or more values or chains than it follows) stopped the
    /// chain that ends there (B10-3).
    Capped,
    /// Today's reading, kept at a caller the library call's reading states
    /// no row and no lead at: a claimed part the caller writes and the
    /// library call does not (`send({ topic, messages })` handed on whole)
    /// (B10-3).
    CallerToday,
    /// The text reader states a text where today's reading states another
    /// at the same place: a row whose name moves (B10-3).
    TextDiffers,
}

impl Word for NameOutcome {
    const ALL: &'static [Self] = &[
        NameOutcome::Literal,
        NameOutcome::Linked,
        NameOutcome::Filled,
        NameOutcome::Made,
        NameOutcome::NoRows,
        NameOutcome::R1Builder,
        NameOutcome::StopSpread,
        NameOutcome::StopUnread,
        NameOutcome::Lead,
        NameOutcome::LeadTodayStated,
        NameOutcome::KindAsked,
        NameOutcome::Capped,
        NameOutcome::CallerToday,
        NameOutcome::TextDiffers,
    ];

    fn word(self) -> &'static str {
        match self {
            NameOutcome::Literal => "literal",
            NameOutcome::Linked => "linked",
            NameOutcome::Filled => "filled",
            NameOutcome::Made => "made",
            NameOutcome::NoRows => "no_rows",
            NameOutcome::R1Builder => "r1_builder",
            NameOutcome::StopSpread => "stop_spread",
            NameOutcome::StopUnread => "stop_unread",
            NameOutcome::Lead => "lead",
            NameOutcome::LeadTodayStated => "lead_today_stated",
            NameOutcome::KindAsked => "kind_asked",
            NameOutcome::Capped => "capped",
            NameOutcome::CallerToday => "caller_today",
            NameOutcome::TextDiffers => "text_differs",
        }
    }
}

/// The library reader's name outcomes, one per site it read a name slot of.
pub type LibraryNames = Tally<NameOutcome>;

/// Where the site path's name for a message site came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameSource {
    /// Every piece is text the labelled call itself writes.
    LiteralPiece,
    /// Every piece is text, and the site is a caller of the function the
    /// labelled call is in: the walk filled a parameter.
    FilledParam,
    /// The name rests on a value the resolver read, or on several.
    ResolverFold,
    /// The name is a slot the model is asked.
    Asked,
    /// The code states no whole name and nothing is asked: a lead, with the
    /// reason the resolver gave.
    Unstated,
    /// A lead whose record holds no reason (`unread: None`):
    /// `possible_calls::reason_of` saves it as `unanswered`, although nothing
    /// was asked (carrick#4446). Every one is a path into that fallback
    /// that has no reason of its own yet.
    NoReason,
}

impl Word for NameSource {
    const ALL: &'static [Self] = &[
        NameSource::LiteralPiece,
        NameSource::FilledParam,
        NameSource::ResolverFold,
        NameSource::Asked,
        NameSource::Unstated,
        NameSource::NoReason,
    ];

    fn word(self) -> &'static str {
        match self {
            NameSource::LiteralPiece => "literal_piece",
            NameSource::FilledParam => "filled_param",
            NameSource::ResolverFold => "resolver_fold",
            NameSource::Asked => "asked",
            NameSource::Unstated => "unstated",
            NameSource::NoReason => "no_reason",
        }
    }
}

/// The site path's name sources, one per message site.
pub type SiteNames = Tally<NameSource>;

/// What the site path holds at a library-reader row's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Parity {
    /// No site row stands at the row's file, line and side.
    Absent,
    /// A site row at the same place has the same key.
    Agrees,
    /// Site rows stand at the same place and none has the same key.
    Disagrees,
}

impl Word for Parity {
    const ALL: &'static [Self] = &[Parity::Absent, Parity::Agrees, Parity::Disagrees];

    fn word(self) -> &'static str {
        match self {
            Parity::Absent => "absent",
            Parity::Agrees => "agrees",
            Parity::Disagrees => "disagrees",
        }
    }
}

/// For each library-reader row, what the site path states at its file, line
/// and side. `site` is the site path's rows before the fold, so a site the
/// model was asked and never answered is absent. Files are read relative to
/// `root`.
pub fn parity(root: &Path, library: &[LibraryRow], site: &[SiteRow]) -> Tally<Parity> {
    let mut out = Tally::default();
    for row in library {
        let (key, side) = row.operation();
        let file = crate::site_rows::relative(root, &row.file.to_string_lossy());
        let here: Vec<&SiteRow> = site
            .iter()
            .filter(|held| held.file == file && held.line == row.line && held.side == side)
            .collect();
        out.note(if here.is_empty() {
            Parity::Absent
        } else if here.iter().any(|held| held.key == key) {
            Parity::Agrees
        } else {
            Parity::Disagrees
        });
    }
    out
}

/// How the site path read each labelled call's name slot (carrick#3504
/// B10-2b): through the resolver's pieces (`resolver`, and `differs_gain`
/// where a render states as text what today's pieces leave unknown, N1), or
/// on today's walk, by why: a value-hop chain (`r1_value_hop`, Q7a), a value
/// that falls back (`r1_fallback`, by the word of the part that does: a
/// parameter stop's, `stated` for a builder handed a parameter, `capped`,
/// `absent` for no term or value read, `off_chain` and `several_values` for
/// values that do not run through the chain as one), a render at the
/// labelled call that differs from today's (`differs_kept`), a hole the
/// summaries read nothing at (`unrendered`, N5), or a construction hop no
/// chain call is made on (`field_unplaced`, N3). One count per call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunNames {
    pub resolver: usize,
    pub r1_value_hop: usize,
    pub r1_fallback: BTreeMap<&'static str, usize>,
    pub differs_kept: usize,
    pub differs_gain: usize,
    pub unrendered: usize,
    pub field_unplaced: usize,
}

/// One labelled call's count in [`DryRunNames`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SitePathRead {
    Resolver,
    ValueHop,
    Fallback(&'static str),
    DiffersKept,
    DiffersGain,
    Unrendered,
    FieldUnplaced,
}

impl DryRunNames {
    pub fn note(&mut self, read: SitePathRead) {
        match read {
            SitePathRead::Resolver => self.resolver += 1,
            SitePathRead::ValueHop => self.r1_value_hop += 1,
            SitePathRead::Fallback(word) => *self.r1_fallback.entry(word).or_default() += 1,
            SitePathRead::DiffersKept => self.differs_kept += 1,
            SitePathRead::DiffersGain => self.differs_gain += 1,
            SitePathRead::Unrendered => self.unrendered += 1,
            SitePathRead::FieldUnplaced => self.field_unplaced += 1,
        }
    }

    /// The fallbacks by word, as the log prints them.
    pub fn fallback_line(&self) -> String {
        self.r1_fallback
            .iter()
            .map(|(word, count)| format!("{word}={count}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// How each maker root's name was read (carrick#3504 B10-2c1): the
/// resolver's caller-chain read of the maker's name slot, beside the site
/// path's own caller walk (`made_with`). `same`: both read the root and agree,
/// so the resolver's values are taken; `gain`: only the resolver read it, so
/// its values are taken; `differs`: both read it and disagree, so today's
/// stand; `fallback`: the resolver's read stops (a hole, a parameter, a cap),
/// so today's stand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MakerNames {
    pub same: usize,
    pub gain: usize,
    pub differs: usize,
    pub fallback: usize,
}

/// The one debug line per service.
pub fn log(
    root: &Path,
    library: &crate::library_claims::LibraryRows,
    site: &crate::site_rows::SiteRows,
) {
    tracing::debug!(
        library = %library.names.line(),
        site = %site.names.line(),
        parity = %parity(root, &library.rows, &site.rows).line(),
        "message names: library-reader sites by name outcome, site-path message sites by name source, library-reader rows by what the site path states at the same place"
    );
    let read = &site.dry_run_names;
    tracing::debug!(
        resolver = read.resolver,
        r1_value_hop = read.r1_value_hop,
        r1_fallback = read.r1_fallback.values().sum::<usize>(),
        r1_fallback_by_word = %read.fallback_line(),
        differs_kept = read.differs_kept,
        differs_gain = read.differs_gain,
        unrendered = read.unrendered,
        field_unplaced = read.field_unplaced,
        "site-path names: labelled calls whose name slot the site path read through the resolver's pieces, or on its own walk, by why"
    );
    let maker = &site.maker_names;
    tracing::debug!(
        same = maker.same,
        gain = maker.gain,
        differs = maker.differs,
        fallback = maker.fallback,
        "site-path maker names: maker roots whose name the resolver's caller read agrees with the site path's walk on, reads where the walk does not, disagrees on, or does not read"
    );
}
