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
    /// A spread stands where the name is, so nothing is read.
    StopSpread,
    /// Anything else the reader cannot read as text.
    StopUnread,
}

impl Word for NameOutcome {
    const ALL: &'static [Self] = &[
        NameOutcome::Literal,
        NameOutcome::Linked,
        NameOutcome::Filled,
        NameOutcome::StopSpread,
        NameOutcome::StopUnread,
    ];

    fn word(self) -> &'static str {
        match self {
            NameOutcome::Literal => "literal",
            NameOutcome::Linked => "linked",
            NameOutcome::Filled => "filled",
            NameOutcome::StopSpread => "stop_spread",
            NameOutcome::StopUnread => "stop_unread",
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
    /// The code states no whole name and nothing is asked: a lead.
    Unstated,
}

impl Word for NameSource {
    const ALL: &'static [Self] = &[
        NameSource::LiteralPiece,
        NameSource::FilledParam,
        NameSource::ResolverFold,
        NameSource::Asked,
        NameSource::Unstated,
    ];

    fn word(self) -> &'static str {
        match self {
            NameSource::LiteralPiece => "literal_piece",
            NameSource::FilledParam => "filled_param",
            NameSource::ResolverFold => "resolver_fold",
            NameSource::Asked => "asked",
            NameSource::Unstated => "unstated",
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

/// The resolver's pieces read of each labelled call's name slot, beside the
/// site path's own (carrick#3504 B10-2a2, the dry run that sizes B10-2b):
/// one count per call of `same` or `differs` (the pieces read spells the
/// name at the labelled call's own function as the site path does, or not),
/// and among them how many would fall back to the site path's walk, by the
/// word of the part that would, and how many hold a chain the cap ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunNames {
    pub same: usize,
    pub differs: usize,
    pub fallback: BTreeMap<&'static str, usize>,
    pub capped: usize,
}

impl DryRunNames {
    pub fn note(&mut self, same: bool, fallback: Option<&'static str>, capped: bool) {
        if same {
            self.same += 1;
        } else {
            self.differs += 1;
        }
        if let Some(word) = fallback {
            *self.fallback.entry(word).or_default() += 1;
        }
        if capped {
            self.capped += 1;
        }
    }

    /// The fallbacks by word, as the log prints them.
    pub fn fallback_line(&self) -> String {
        self.fallback
            .iter()
            .map(|(word, count)| format!("{word}={count}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
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
    let dry_run = &site.dry_run_names;
    tracing::debug!(
        same = dry_run.same,
        differs = dry_run.differs,
        fallback = dry_run.fallback.values().sum::<usize>(),
        fallback_by_word = %dry_run.fallback_line(),
        capped = dry_run.capped,
        "site-path names (dry run): labelled calls whose name slot the resolver's pieces read spells as the site path does, or not; of them, those that would fall back to the site path's walk, by word, and those a cap ended"
    );
}
