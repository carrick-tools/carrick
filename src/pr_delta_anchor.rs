//! Which operations a pull request's endpoint delta lists (carrick#3080).
//!
//! The key comparison (`engine::delta_candidates`) yields every operation this
//! scan states and main's index lacks, and every stored operation this scan
//! lacks. It compares two readings, not two trees, so some of those rows are
//! a reading that moved rather than the PR's change. This module lists a
//! candidate only when the PR's own diff places it, and counts the rest.
//! Every protocol is covered: a row of any protocol carries its place as
//! `"<file>:<line>"`. The rules are in `docs/reference/runner.md`, "The
//! endpoint delta".
//!
//! [`anchor`] is the pure rule: no git, and a diff already read. [`run`]
//! reads that diff and main's rows' places from git, then calls it.

use crate::git_state::{Hunk, changed_hunks, changed_paths, map_line};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use tracing::debug;

/// Where one operation row sits: the service that states it, its file
/// relative to the scan root, and the line (0 when the row names none).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Place {
    pub service: Option<String>,
    pub file: String,
    pub line: u32,
}

impl Place {
    /// The place a row's `file_path` (`"<file>:<line>"`, or a bare file)
    /// names for `service`.
    pub fn parse(service: &Option<String>, file_path: &str) -> Place {
        let (file, line) = match crate::handler_span::split_location(file_path) {
            Some(located) => located,
            None => (
                crate::handler_span::normalize(file_path.strip_suffix(":0").unwrap_or(file_path)),
                0,
            ),
        };
        Place {
            service: service.clone(),
            file,
            line,
        }
    }
}

/// An operation the key comparison yields, with every place its rows sit at.
#[derive(Debug, Clone)]
pub struct Candidate<T> {
    pub item: T,
    pub places: Vec<Place>,
}

/// What the PR did to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiff {
    Added,
    Deleted,
    /// Edited: the hunks from the merge base to the working tree.
    Modified(Vec<Hunk>),
}

/// The PR's diff, for the files that hold a candidate row. A file absent from
/// it is untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff(pub BTreeMap<String, FileDiff>);

/// Where main's row sits at the merge base `B`, given that main stored it at
/// its own commit `M`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtBase {
    /// The line at `B`.
    At(u32),
    /// Main changed that line between `M` and `B`: main's, not the PR's.
    Moved,
    /// Git cannot read the file at `M` (a clone that did not fetch it, or a
    /// commit it lacks).
    Unreadable,
}

/// One row of main's index with its place at the merge base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainRow {
    /// The place as main stored it.
    pub place: Place,
    pub at_base: AtBase,
    /// Whether main stored the row at the merge base itself. A row stored at
    /// another commit may be one main removed itself since, by an edit to
    /// another file, so its absence is never read as this scan's loss.
    pub stored_at_base: bool,
}

/// Why a candidate was left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    EditedFile,
    UntouchedFile,
    MainMoved,
    PlacedByFile,
    ReadingMoved,
}

impl Reason {
    fn label(self) -> &'static str {
        match self {
            Reason::EditedFile => "edited_file",
            Reason::UntouchedFile => "untouched_file",
            Reason::MainMoved => "main_moved",
            Reason::PlacedByFile => "placed_by_file",
            Reason::ReadingMoved => "reading_moved",
        }
    }
}

/// How many candidates were left out, by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeftOut {
    pub edited_file: usize,
    pub untouched_file: usize,
    pub main_moved: usize,
    pub placed_by_file: usize,
    pub reading_moved: usize,
}

impl LeftOut {
    pub fn total(&self) -> usize {
        self.edited_file
            + self.untouched_file
            + self.main_moved
            + self.placed_by_file
            + self.reading_moved
    }

    fn count(&mut self, reason: Reason) {
        match reason {
            Reason::EditedFile => self.edited_file += 1,
            Reason::UntouchedFile => self.untouched_file += 1,
            Reason::MainMoved => self.main_moved += 1,
            Reason::PlacedByFile => self.placed_by_file += 1,
            Reason::ReadingMoved => self.reading_moved += 1,
        }
    }
}

/// The candidates the PR's diff places, and what was left out.
#[derive(Debug)]
pub struct Anchored<T> {
    pub new: Vec<T>,
    pub removed: Vec<T>,
    pub left_out: LeftOut,
    /// Of the route sites this scan could not read a path for that the
    /// caller named, those on a line the PR added or changed, or in a file it
    /// added: registrations the PR wrote in a form the scan cannot read, so
    /// a removal they could serve is not proven (carrick#3222).
    pub unread_written: Vec<Place>,
    /// The removed candidates left out on a line the PR did not change, in a
    /// service whose reading did not move, that main stored at the merge base
    /// itself: main held the row there and this scan does not state it, so
    /// the scan lost it (carrick#3233), and those placed only in a changed
    /// file this scan could not read. Not listed, and counted in
    /// `left_out` as before; the delta names their files instead.
    pub lost: Vec<Candidate<T>>,
    /// Of the source files this scan could not read that the caller named,
    /// those the PR added or edited (carrick#3233).
    pub unread_changed: Vec<Place>,
}

impl<T> Anchored<T> {
    /// The one info line a PR run logs.
    pub fn summary(&self) -> String {
        let placed = format!(
            "{} new and {} removed operation(s) placed by this PR's diff",
            self.new.len(),
            self.removed.len()
        );
        let out = &self.left_out;
        if out.total() == 0 {
            return format!("PR delta: {placed}");
        }
        let parts: Vec<String> = [
            (out.edited_file, "in files this PR edits"),
            (out.untouched_file, "in files it does not touch"),
            (out.main_moved, "main's index holds at another commit"),
            (
                out.placed_by_file,
                "in a file git could not read at main's commit",
            ),
            (
                out.reading_moved,
                "in a service whose dependencies or config this PR changes",
            ),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{count} {what}"))
        .collect();
        format!(
            "PR delta: {placed}; {} left out because the diff does not place them ({})",
            out.total(),
            parts.join(", ")
        )
    }
}

/// Whether the PR added or changed line `line`.
fn on_changed_line(hunks: &[Hunk], line: u32) -> bool {
    hunks.iter().any(|hunk| {
        hunk.new_count > 0 && line >= hunk.new_start && line < hunk.new_start + hunk.new_count
    })
}

/// What the rules read: the diff, main's rows, and this scan's rows.
struct Context<'a> {
    diff: &'a Diff,
    main: &'a [MainRow],
    scan: &'a [Place],
    /// The services whose inputs other than source files this PR changes:
    /// their reading may have moved with no edit to the file that states a
    /// route, so the same-place passes do not list their rows.
    reading_moved: &'a BTreeSet<Option<String>>,
}

impl Context<'_> {
    /// Whether main's index holds a row of `place`'s service in the same
    /// file, at the line `place.line` once main's line is moved through the
    /// PR's diff. Its key differs, or the candidate would not be one.
    fn main_holds_same_place(&self, place: &Place) -> bool {
        self.main.iter().any(|row| {
            row.place.service == place.service
                && row.place.file == place.file
                && match row.at_base {
                    AtBase::At(line) if line > 0 => {
                        self.moved_through_diff(&place.file, line) == Some(place.line)
                    }
                    _ => false,
                }
        })
    }

    /// Whether this scan holds a row of `place`'s service at exactly `place`.
    fn scan_holds(&self, place: &Place) -> bool {
        self.scan.iter().any(|row| row == place)
    }

    /// Whether the PR wrote `place`'s line: it added the file, or added or
    /// changed that line. A place with no line is never written.
    fn written(&self, place: &Place) -> bool {
        match self.diff.0.get(&place.file) {
            Some(FileDiff::Added) => true,
            Some(FileDiff::Modified(hunks)) => place.line > 0 && on_changed_line(hunks, place.line),
            _ => false,
        }
    }

    /// Line `line` of `file` at the merge base, in the working tree.
    fn moved_through_diff(&self, file: &str, line: u32) -> Option<u32> {
        match self.diff.0.get(file) {
            None => Some(line),
            Some(FileDiff::Modified(hunks)) => map_line(hunks, line),
            Some(FileDiff::Added | FileDiff::Deleted) => None,
        }
    }

    /// Whether the same-place pass lists a row of `place`'s service.
    fn same_place_allowed(&self, place: &Place) -> Result<(), Reason> {
        if self.reading_moved.contains(&place.service) {
            Err(Reason::ReadingMoved)
        } else {
            Ok(())
        }
    }

    fn judge_new(&self, place: &Place) -> Result<(), Reason> {
        let change = self.diff.0.get(&place.file);
        match change {
            Some(FileDiff::Added) => return Ok(()),
            Some(FileDiff::Modified(_)) if place.line == 0 => return Ok(()),
            Some(FileDiff::Modified(hunks)) if on_changed_line(hunks, place.line) => return Ok(()),
            _ => {}
        }
        if place.line > 0 && self.main_holds_same_place(place) {
            return self.same_place_allowed(place);
        }
        Err(match change {
            None => Reason::UntouchedFile,
            Some(_) => Reason::EditedFile,
        })
    }

    fn judge_removed(&self, row: &MainRow) -> Result<(), Reason> {
        let place = &row.place;
        let change = self.diff.0.get(&place.file);
        if place.line == 0 {
            return change.map(|_| ()).ok_or(Reason::UntouchedFile);
        }
        let at = match row.at_base {
            AtBase::Moved => return Err(Reason::MainMoved),
            AtBase::Unreadable => {
                return change.map(|_| ()).ok_or(Reason::PlacedByFile);
            }
            AtBase::At(line) => line,
        };
        let at_base = Place {
            line: at,
            ..place.clone()
        };
        match change {
            Some(FileDiff::Deleted | FileDiff::Added) => Ok(()),
            Some(FileDiff::Modified(hunks)) => match map_line(hunks, at) {
                None => Ok(()),
                Some(line)
                    if self.scan_holds(&Place {
                        line,
                        ..at_base.clone()
                    }) =>
                {
                    self.same_place_allowed(place)
                }
                Some(_) => Err(Reason::EditedFile),
            },
            None if self.scan_holds(&at_base) => self.same_place_allowed(place),
            None => Err(Reason::UntouchedFile),
        }
    }
}

/// The candidates of `new` and `removed` that the PR's `diff` places.
///
/// A candidate is listed when any one of its rows passes. A new row passes
/// when the PR adds its file, when its line is one the PR added or changed,
/// or when main's index holds a row of the same service at the same place
/// (an edit elsewhere changed what that registration resolves to: a mount
/// prefix, an imported path constant, or the path literal below a multi-line
/// registration's opening line). A removed row passes when the PR deletes its
/// file or replaces its line, or when this scan holds a row of the same
/// service at the place the PR's hunks move it to. A row main changed itself
/// between its stored commit and the merge base never passes. A row with no
/// line passes when the PR touches its file. `scan` is every row this scan
/// states and `main` every row of main's index in a file that holds a
/// candidate. Those same-place passes are closed for a service in
/// `reading_moved`: a PR that changes an input of the service other than
/// source files (carrick#1712).
/// candidate. Of `unread`, the route sites this scan could not read a path
/// for, the result keeps those the PR wrote (`unread_written`). Of
/// `unread_sources`, the source files this scan could not read, it keeps
/// those the PR added or edited (`unread_changed`). A removed candidate left
/// out on a line main stored at the merge base is `lost` ([`is_lost`]).
#[allow(clippy::too_many_arguments)]
pub fn anchor<T>(
    new: Vec<Candidate<T>>,
    removed: Vec<Candidate<T>>,
    scan: &[Place],
    main: &[MainRow],
    diff: &Diff,
    reading_moved: &BTreeSet<Option<String>>,
    unread: &[Place],
    unread_sources: &[Place],
) -> Anchored<T> {
    let context = Context {
        diff,
        main,
        scan,
        reading_moved,
    };
    let unread_written = unread
        .iter()
        .filter(|place| context.written(place))
        .cloned()
        .collect();
    let unread_changed: Vec<Place> = unread_sources
        .iter()
        .filter(|place| {
            matches!(
                diff.0.get(&place.file),
                Some(FileDiff::Added | FileDiff::Modified(_))
            )
        })
        .cloned()
        .collect();
    let mut lost = Vec::new();
    let mut left_out = LeftOut::default();
    let mut listed_new = Vec::new();
    for candidate in new {
        match judge(&candidate, |place| context.judge_new(place)) {
            Ok(()) => listed_new.push(candidate.item),
            Err(reason) => {
                debug!(
                    "PR delta: new operation at {:?} left out ({})",
                    candidate.places,
                    reason.label()
                );
                left_out.count(reason);
            }
        }
    }
    let mut listed_removed = Vec::new();
    for candidate in removed {
        let rows: Vec<MainRow> = candidate
            .places
            .iter()
            .map(|place| {
                main.iter()
                    .find(|row| &row.place == place)
                    .cloned()
                    .unwrap_or_else(|| MainRow {
                        place: place.clone(),
                        at_base: AtBase::At(place.line),
                        // Main's commit for it is unknown: never lost.
                        stored_at_base: false,
                    })
            })
            .collect();
        let mut verdict = Err(Reason::UntouchedFile);
        for (index, row) in rows.iter().enumerate() {
            match context.judge_removed(row) {
                Ok(()) => {
                    verdict = Ok(());
                    break;
                }
                Err(reason) if index == 0 => verdict = Err(reason),
                Err(_) => {}
            }
        }
        match verdict {
            // A file this scan could not read states none of its rows, so a
            // removal placed only in such a file is unproven: named with the
            // file, never listed.
            Ok(()) if all_in_unread_files(&candidate.places, &unread_changed) => {
                debug!(
                    "PR delta: removed operation at {:?} is not listed: its file could not be read",
                    candidate.places
                );
                lost.push(candidate);
            }
            Ok(()) => listed_removed.push(candidate.item),
            Err(reason) => {
                debug!(
                    "PR delta: removed operation at {:?} left out ({})",
                    candidate.places,
                    reason.label()
                );
                left_out.count(reason);
                if rows
                    .first()
                    .is_some_and(|row| is_lost(row, reason, reading_moved))
                {
                    lost.push(candidate);
                }
            }
        }
    }
    Anchored {
        new: listed_new,
        removed: listed_removed,
        left_out,
        unread_written,
        lost,
        unread_changed,
    }
}

/// Whether every place of a candidate is in one of the `unread` files.
fn all_in_unread_files(places: &[Place], unread: &[Place]) -> bool {
    !places.is_empty()
        && places.iter().all(|place| {
            unread
                .iter()
                .any(|file| file.service == place.service && file.file == place.file)
        })
}

/// Whether a removed candidate left out for `reason`, judged on `row` (its
/// first place), is a row this scan lost: main held it at the merge base on a
/// line the PR did not change, in a service whose reading did not move, and
/// this scan does not state it there (carrick#3233). A row main moved itself,
/// one git could not place, one with no line, and one in a service whose
/// reading moved are never lost; so is one main stored at another commit,
/// which may be main's own removal since (logged).
fn is_lost(row: &MainRow, reason: Reason, reading_moved: &BTreeSet<Option<String>>) -> bool {
    let left_on_its_line = matches!(reason, Reason::EditedFile | Reason::UntouchedFile)
        && row.place.line > 0
        && matches!(row.at_base, AtBase::At(_))
        && !reading_moved.contains(&row.place.service);
    if left_on_its_line && !row.stored_at_base {
        debug!(
            "PR delta: removed operation at {:?} not counted as lost: main stored it at \
             another commit than the merge base",
            row.place
        );
    }
    left_on_its_line && row.stored_at_base
}

/// `Ok` when any place of `candidate` passes `rule`; else the reason the
/// first place failed (a candidate with no place is in no file).
fn judge<T>(
    candidate: &Candidate<T>,
    rule: impl Fn(&Place) -> Result<(), Reason>,
) -> Result<(), Reason> {
    let mut first = None;
    for place in &candidate.places {
        match rule(place) {
            Ok(()) => return Ok(()),
            Err(reason) => {
                first.get_or_insert(reason);
            }
        }
    }
    Err(first.unwrap_or(Reason::UntouchedFile))
}

/// The paths git names (relative to the repository's top level) as paths
/// relative to the scan root, the form a row carries; `None` for one outside
/// the scan root.
struct RootPaths {
    top: PathBuf,
    root: PathBuf,
}

impl RootPaths {
    fn of(repo_path: &str) -> Result<Self, String> {
        let canonical = |path: PathBuf| path.canonicalize().unwrap_or(path);
        Ok(RootPaths {
            top: canonical(crate::git_state::toplevel(repo_path)?),
            root: canonical(PathBuf::from(repo_path)),
        })
    }

    fn scan_relative(&self, git_path: &str) -> Option<String> {
        crate::pr_functions::blob_path(&self.top.join(git_path), &self.root)
    }
}

/// The PR's diff for `files`: from the merge base `base` to the working tree
/// the scan read, untracked files counted as added.
fn read_diff(repo_path: &str, base: &str, files: &BTreeSet<String>) -> Result<Diff, String> {
    let paths = RootPaths::of(repo_path)?;
    let mut diff = BTreeMap::new();
    for changed in changed_paths(repo_path, base, None)? {
        let Some(file) = paths.scan_relative(&changed.path) else {
            continue;
        };
        if !files.contains(&file) {
            continue;
        }
        let change = match changed.status {
            'A' => FileDiff::Added,
            'D' => FileDiff::Deleted,
            _ => FileDiff::Modified(changed_hunks(repo_path, base, &file, None)?),
        };
        diff.insert(file, change);
    }
    Ok(Diff(diff))
}

/// A row of main's index and the commit its service was stored at.
#[derive(Debug, Clone)]
pub struct MainSource {
    pub place: Place,
    pub commit: String,
}

/// Where each of `rows` sits at the merge base `base`. A row stored at
/// `base` stays; one stored at another commit moves through the hunks from
/// that commit to `base` when its file differs between them.
fn place_main_rows(repo_path: &str, base: &str, rows: Vec<MainSource>) -> Vec<MainRow> {
    let paths = RootPaths::of(repo_path).ok();
    let mut differing: BTreeMap<String, Option<BTreeMap<String, char>>> = BTreeMap::new();
    let mut hunks: BTreeMap<(String, String), Option<Vec<Hunk>>> = BTreeMap::new();
    rows.into_iter()
        .map(|MainSource { place, commit }| {
            let at_base = if commit == base || place.line == 0 {
                AtBase::At(place.line)
            } else {
                let changes = differing.entry(commit.clone()).or_insert_with(|| {
                    let paths = paths.as_ref()?;
                    let listed = changed_paths(repo_path, &commit, Some(base)).ok()?;
                    Some(
                        listed
                            .into_iter()
                            .filter_map(|c| Some((paths.scan_relative(&c.path)?, c.status)))
                            .collect(),
                    )
                });
                match changes {
                    None => AtBase::Unreadable,
                    Some(changes) => match changes.get(&place.file) {
                        None => AtBase::At(place.line),
                        Some('D') => AtBase::Moved,
                        Some('A') => AtBase::Unreadable,
                        Some(_) => {
                            let read = hunks
                                .entry((commit.clone(), place.file.clone()))
                                .or_insert_with(|| {
                                    changed_hunks(repo_path, &commit, &place.file, Some(base)).ok()
                                });
                            match read {
                                None => AtBase::Unreadable,
                                Some(read) => match map_line(read, place.line) {
                                    Some(line) => AtBase::At(line),
                                    None => AtBase::Moved,
                                },
                            }
                        }
                    },
                }
            };
            MainRow {
                place,
                at_base,
                stored_at_base: commit == base,
            }
        })
        .collect()
}

/// [`anchor`] over the diff git gives for the checkout at `repo_path`.
///
/// The diff runs from the merge base of `pr_base` and HEAD to the working
/// tree: on a merge checkout the merge base is `pr_base`, and on a head
/// checkout it is the fork point, so commits main gained after the PR
/// branched are never read as the PR's (as `pr_functions::block_for_run`
/// reads it). Git is asked only about files that hold a candidate row or one
/// of `unread` or `unread_sources`; a run with no candidate and no unread
/// source makes no git call. `Err` means git could not read the diff.
#[allow(clippy::too_many_arguments)]
pub fn run<T>(
    repo_path: &str,
    pr_base: &str,
    new: Vec<Candidate<T>>,
    removed: Vec<Candidate<T>>,
    scan: &[Place],
    main: Vec<MainSource>,
    reading_moved: impl FnOnce(&str) -> BTreeSet<Option<String>>,
    unread: &[Place],
    unread_sources: &[Place],
) -> Result<Anchored<T>, String> {
    if new.is_empty() && removed.is_empty() && unread_sources.is_empty() {
        return Ok(anchor(
            new,
            removed,
            scan,
            &[],
            &Diff::default(),
            &BTreeSet::new(),
            unread,
            unread_sources,
        ));
    }
    let files: BTreeSet<String> = new
        .iter()
        .chain(&removed)
        .flat_map(|candidate| candidate.places.iter().map(|place| place.file.clone()))
        .collect();
    let mut diff_files = files.clone();
    diff_files.extend(
        unread
            .iter()
            .chain(unread_sources)
            .map(|place| place.file.clone()),
    );
    let head = crate::git_state::head_commit(repo_path)?;
    let merge_base = crate::git_state::merge_base(repo_path, pr_base, &head)?;
    let diff = read_diff(repo_path, &merge_base, &diff_files)?;
    let in_files: Vec<MainSource> = main
        .into_iter()
        .filter(|row| files.contains(&row.place.file))
        .collect();
    let main = place_main_rows(repo_path, &merge_base, in_files);
    let moved = reading_moved(&merge_base);
    Ok(anchor(
        new,
        removed,
        scan,
        &main,
        &diff,
        &moved,
        unread,
        unread_sources,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_state::tests::{committed_repo, git_in};

    fn place(file: &str, line: u32) -> Place {
        Place {
            service: Some("web".to_string()),
            file: file.to_string(),
            line,
        }
    }

    fn candidate(item: &'static str, places: &[(&str, u32)]) -> Candidate<&'static str> {
        Candidate {
            item,
            places: places.iter().map(|(f, l)| place(f, *l)).collect(),
        }
    }

    fn hunk(old_start: u32, old_count: u32, new_start: u32, new_count: u32) -> Hunk {
        Hunk {
            old_start,
            old_count,
            new_start,
            new_count,
        }
    }

    fn diff(entries: Vec<(&str, FileDiff)>) -> Diff {
        Diff(
            entries
                .into_iter()
                .map(|(file, change)| (file.to_string(), change))
                .collect(),
        )
    }

    fn main_row(file: &str, line: u32, at_base: AtBase) -> MainRow {
        MainRow {
            place: place(file, line),
            at_base,
            stored_at_base: true,
        }
    }

    /// The listed new items, the listed removed items and what was left out.
    fn listed(
        new: Vec<Candidate<&'static str>>,
        removed: Vec<Candidate<&'static str>>,
        scan: &[Place],
        main: &[MainRow],
        diff: &Diff,
    ) -> (Vec<&'static str>, Vec<&'static str>, LeftOut) {
        listed_moved(new, removed, scan, main, diff, &BTreeSet::new())
    }

    /// [`listed`] with the services whose reading moved.
    fn listed_moved(
        new: Vec<Candidate<&'static str>>,
        removed: Vec<Candidate<&'static str>>,
        scan: &[Place],
        main: &[MainRow],
        diff: &Diff,
        reading_moved: &BTreeSet<Option<String>>,
    ) -> (Vec<&'static str>, Vec<&'static str>, LeftOut) {
        let anchored = anchor(new, removed, scan, main, diff, reading_moved, &[], &[]);
        (anchored.new, anchored.removed, anchored.left_out)
    }

    /// The service of [`place`], whose reading moved.
    fn web_moved() -> BTreeSet<Option<String>> {
        BTreeSet::from([Some("web".to_string())])
    }

    fn new_only(
        rows: &[(&str, u32)],
        scan: &[Place],
        main: &[MainRow],
        diff: &Diff,
    ) -> (bool, LeftOut) {
        let (new, _, out) = listed(vec![candidate("x", rows)], vec![], scan, main, diff);
        (!new.is_empty(), out)
    }

    fn removed_only(row: MainRow, scan: &[Place], diff: &Diff) -> (bool, LeftOut) {
        let candidate = Candidate {
            item: "x",
            places: vec![row.place.clone()],
        };
        let (_, removed, out) = listed(vec![], vec![candidate], scan, &[row], diff);
        (!removed.is_empty(), out)
    }

    const ROUTES: &str = "src/routes.ts";

    #[test]
    fn a_new_row_in_an_added_file_is_listed() {
        let d = diff(vec![(ROUTES, FileDiff::Added)]);
        assert_eq!(
            new_only(&[(ROUTES, 4)], &[], &[], &d),
            (true, LeftOut::default())
        );
    }

    #[test]
    fn a_new_row_on_an_added_or_changed_line_is_listed() {
        let d = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(9, 1, 9, 3)]))]);
        for line in [9, 10, 11] {
            assert!(new_only(&[(ROUTES, line)], &[], &[], &d).0, "line {line}");
        }
        // A pure deletion changes no line of the working tree.
        let d = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(9, 2, 8, 0)]))]);
        assert!(!new_only(&[(ROUTES, 9)], &[], &[], &d).0);
    }

    #[test]
    fn a_new_row_outside_every_hunk_of_an_edited_file_is_left_out() {
        let d = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(2, 1, 2, 1)]))]);
        let (listed, out) = new_only(&[(ROUTES, 30)], &[], &[], &d);
        assert!(!listed);
        assert_eq!(
            out,
            LeftOut {
                edited_file: 1,
                ..LeftOut::default()
            }
        );
    }

    #[test]
    fn a_new_row_in_an_untouched_file_is_left_out() {
        let (listed, out) = new_only(&[(ROUTES, 30)], &[], &[], &Diff::default());
        assert!(!listed);
        assert_eq!(
            out,
            LeftOut {
                untouched_file: 1,
                ..LeftOut::default()
            }
        );
    }

    #[test]
    fn an_edit_elsewhere_that_changes_a_key_lists_both_rows() {
        // The PR edits only a mount prefix in another file: the routes file
        // is untouched, and main's row and this scan's row share line 10.
        let d = diff(vec![(
            "src/app.ts",
            FileDiff::Modified(vec![hunk(3, 1, 3, 1)]),
        )]);
        let main = [main_row(ROUTES, 10, AtBase::At(10))];
        let scan = [place(ROUTES, 10)];
        let (new, removed, out) = listed(
            vec![candidate("/api/v2/x", &[(ROUTES, 10)])],
            vec![Candidate {
                item: "/api/x",
                places: vec![place(ROUTES, 10)],
            }],
            &scan,
            &main,
            &d,
        );
        assert_eq!(
            (new, removed, out),
            (vec!["/api/v2/x"], vec!["/api/x"], LeftOut::default())
        );
    }

    #[test]
    fn a_path_literal_changed_below_a_multi_line_registration_lists_both_rows() {
        // Two lines are inserted at the top and the literal on line 11 (now
        // 13) changes under a registration that opens on line 10 (now 12).
        let d = diff(vec![(
            ROUTES,
            FileDiff::Modified(vec![hunk(0, 0, 1, 2), hunk(11, 1, 13, 1)]),
        )]);
        let main = [main_row(ROUTES, 10, AtBase::At(10))];
        let scan = [place(ROUTES, 12)];
        let (new, removed, _) = listed(
            vec![candidate("/new", &[(ROUTES, 12)])],
            vec![Candidate {
                item: "/old",
                places: vec![place(ROUTES, 10)],
            }],
            &scan,
            &main,
            &d,
        );
        assert_eq!((new, removed), (vec!["/new"], vec!["/old"]));
    }

    #[test]
    fn a_removed_row_in_a_deleted_file_is_listed() {
        let d = diff(vec![(ROUTES, FileDiff::Deleted)]);
        assert!(removed_only(main_row(ROUTES, 5, AtBase::At(5)), &[], &d).0);
    }

    #[test]
    fn a_removed_row_on_a_line_the_pr_replaces_is_listed() {
        let d = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(5, 1, 5, 2)]))]);
        assert!(removed_only(main_row(ROUTES, 5, AtBase::At(5)), &[], &d).0);
    }

    #[test]
    fn a_removed_row_on_an_untouched_line_is_left_out() {
        let d = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(40, 1, 40, 1)]))]);
        let (listed, out) = removed_only(main_row(ROUTES, 5, AtBase::At(5)), &[], &d);
        assert!(!listed);
        assert_eq!(out.edited_file, 1);
        let (listed, out) = removed_only(main_row(ROUTES, 5, AtBase::At(5)), &[], &Diff::default());
        assert!(!listed);
        assert_eq!(out.untouched_file, 1);
    }

    /// The removed items `lost`, and what was left out, for one removed
    /// candidate at `row` with the services in `reading_moved`.
    fn lost_of(
        row: MainRow,
        diff: &Diff,
        reading_moved: &BTreeSet<Option<String>>,
    ) -> (Vec<&'static str>, LeftOut) {
        let candidate = Candidate {
            item: "x",
            places: vec![row.place.clone()],
        };
        let anchored = anchor(
            vec![],
            vec![candidate],
            &[],
            &[row],
            diff,
            reading_moved,
            &[],
            &[],
        );
        assert!(anchored.removed.is_empty(), "a lost row is never listed");
        (
            anchored.lost.into_iter().map(|c| c.item).collect(),
            anchored.left_out,
        )
    }

    /// carrick#3233: main held the row at the merge base on a line the PR
    /// did not change, and this scan does not state it: the scan lost it.
    /// It is still left out and counted as before.
    #[test]
    fn a_removed_row_on_an_untouched_line_is_lost() {
        let edited = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(40, 1, 40, 1)]))]);
        let none = BTreeSet::new();
        let (lost, out) = lost_of(main_row(ROUTES, 5, AtBase::At(5)), &edited, &none);
        assert_eq!(lost, vec!["x"]);
        assert_eq!(
            out,
            LeftOut {
                edited_file: 1,
                ..LeftOut::default()
            }
        );
        let (lost, out) = lost_of(main_row(ROUTES, 5, AtBase::At(5)), &Diff::default(), &none);
        assert_eq!(lost, vec!["x"]);
        assert_eq!(
            out,
            LeftOut {
                untouched_file: 1,
                ..LeftOut::default()
            }
        );
    }

    /// A row main moved itself, one git could not place, one with no line
    /// and one in a service whose reading moved are left out as before, and
    /// none is lost.
    #[test]
    fn a_moved_unreadable_line_zero_or_reading_moved_row_is_never_lost() {
        let edited = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(40, 1, 40, 1)]))]);
        let none = BTreeSet::new();
        for (row, d) in [
            (main_row(ROUTES, 5, AtBase::Moved), &edited),
            (main_row(ROUTES, 5, AtBase::Unreadable), &Diff::default()),
            (main_row(ROUTES, 0, AtBase::At(0)), &Diff::default()),
        ] {
            let (lost, out) = lost_of(row.clone(), d, &none);
            assert!(lost.is_empty(), "{row:?} is not lost");
            assert_eq!(out.total(), 1);
        }
        let (lost, out) = lost_of(main_row(ROUTES, 5, AtBase::At(5)), &edited, &web_moved());
        assert!(lost.is_empty(), "the service's reading moved");
        assert_eq!(out.edited_file, 1);
    }

    /// P2: main stored the row at a commit before the merge base. The line is
    /// unchanged at the merge base and this scan lacks the row, but main may
    /// have removed it itself since, by an edit to another file.
    #[test]
    fn a_row_main_stored_before_the_merge_base_is_never_lost() {
        let row = MainRow {
            stored_at_base: false,
            ..main_row(ROUTES, 5, AtBase::At(5))
        };
        let edited = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(40, 1, 40, 1)]))]);
        let (lost, out) = lost_of(row, &edited, &BTreeSet::new());
        assert!(lost.is_empty());
        assert_eq!(out.edited_file, 1);
    }

    /// Of the source files this scan could not read, the delta keeps those
    /// the PR added or edited; a deleted or untouched one is not the PR's.
    #[test]
    fn an_unread_source_the_pr_added_or_edited_is_kept() {
        let d = diff(vec![
            ("src/added.ts", FileDiff::Added),
            ("src/edited.ts", FileDiff::Modified(vec![hunk(3, 1, 3, 1)])),
            ("src/deleted.ts", FileDiff::Deleted),
        ]);
        let sources = [
            place("src/added.ts", 0),
            place("src/edited.ts", 0),
            place("src/deleted.ts", 0),
            place("src/untouched.ts", 0),
        ];
        let anchored: Anchored<&str> = anchor(
            vec![],
            vec![],
            &[],
            &[],
            &d,
            &BTreeSet::new(),
            &[],
            &sources,
        );
        assert_eq!(
            anchored.unread_changed,
            vec![place("src/added.ts", 0), place("src/edited.ts", 0)]
        );
    }

    /// A run with no candidate still reads the diff for an unread source.
    #[test]
    fn the_reader_reads_a_file_that_holds_only_an_unread_source() {
        let (repo, base) = committed_repo(&[("broken.ts", "a\n"), ("kept.ts", "k\n")]);
        let dir = repo.path();
        std::fs::write(dir.join("broken.ts"), "a\nfunction (\n").unwrap();
        let anchored: Anchored<&str> = run(
            &dir.to_string_lossy(),
            &base,
            vec![],
            vec![],
            &[],
            vec![],
            |_| BTreeSet::new(),
            &[],
            &[place("broken.ts", 0), place("kept.ts", 0)],
        )
        .unwrap();
        assert_eq!(anchored.unread_changed, vec![place("broken.ts", 0)]);
    }

    #[test]
    fn a_removed_row_moves_through_the_hunks_to_find_its_scan_row() {
        // Two lines inserted above: main's line 5 is line 7 now, and this
        // scan holds a row there under another key.
        let d = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(0, 0, 1, 2)]))]);
        assert!(removed_only(main_row(ROUTES, 5, AtBase::At(5)), &[place(ROUTES, 7)], &d).0);
        assert!(!removed_only(main_row(ROUTES, 5, AtBase::At(5)), &[place(ROUTES, 5)], &d).0);
    }

    #[test]
    fn a_row_main_changed_itself_is_left_out() {
        let (listed, out) = removed_only(
            main_row(ROUTES, 5, AtBase::Moved),
            &[],
            &diff(vec![(ROUTES, FileDiff::Deleted)]),
        );
        assert!(!listed);
        assert_eq!(out.main_moved, 1);
    }

    #[test]
    fn a_row_git_cannot_read_at_mains_commit_is_placed_by_its_file() {
        let modified = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(40, 1, 40, 1)]))]);
        assert!(removed_only(main_row(ROUTES, 5, AtBase::Unreadable), &[], &modified).0);
        let (listed, out) = removed_only(
            main_row(ROUTES, 5, AtBase::Unreadable),
            &[],
            &Diff::default(),
        );
        assert!(!listed);
        assert_eq!(out.placed_by_file, 1);
    }

    #[test]
    fn a_row_with_no_line_is_placed_by_its_file() {
        let modified = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(40, 1, 40, 1)]))]);
        assert!(new_only(&[(ROUTES, 0)], &[], &[], &modified).0);
        assert!(!new_only(&[(ROUTES, 0)], &[], &[], &Diff::default()).0);
        assert!(removed_only(main_row(ROUTES, 0, AtBase::At(0)), &[], &modified).0);
        assert!(!removed_only(main_row(ROUTES, 0, AtBase::At(0)), &[], &Diff::default()).0);
    }

    #[test]
    fn every_protocol_is_placed_by_the_same_rule() {
        // A GraphQL field at its resolver, a socket event and a pub/sub topic
        // are rows at `file:line` like a route: listed on an added line, left
        // out in an untouched file.
        let added = diff(vec![(
            "src/schema.ts",
            FileDiff::Modified(vec![hunk(20, 0, 21, 3)]),
        )]);
        for (file, line) in [
            ("src/schema.ts", 22),
            ("src/events.ts", 8),
            ("src/bus.ts", 14),
        ] {
            let touched = diff(vec![(
                file,
                FileDiff::Modified(vec![hunk(line - 1, 0, line, 1)]),
            )]);
            assert!(new_only(&[(file, line)], &[], &[], &touched).0, "{file}");
            assert!(
                !new_only(&[(file, line)], &[], &[], &Diff::default()).0,
                "{file}"
            );
        }
        assert!(new_only(&[("src/schema.ts", 22)], &[], &[], &added).0);
    }

    #[test]
    fn a_key_with_two_rows_one_placed_is_listed_once() {
        let d = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(9, 1, 9, 1)]))]);
        let (new, _, out) = listed(
            vec![candidate("x", &[(ROUTES, 50), (ROUTES, 9)])],
            vec![],
            &[],
            &[],
            &d,
        );
        assert_eq!(new, vec!["x"]);
        assert_eq!(out.total(), 0);
    }

    /// carrick#3222: of the route sites the scan could not read a path for,
    /// the result keeps those the PR wrote: on a line it added or changed, or
    /// in a file it added. One on a line it left alone, or in a file it does
    /// not touch, is a registration main read the same way.
    #[test]
    fn an_unread_route_site_on_a_line_the_pr_wrote_is_kept() {
        const PLUGIN: &str = "src/plugin.ts";
        const OTHER: &str = "src/other.ts";
        let d = diff(vec![
            (ROUTES, FileDiff::Modified(vec![hunk(9, 1, 9, 2)])),
            (PLUGIN, FileDiff::Added),
        ]);
        let unread = [
            place(ROUTES, 10),
            place(ROUTES, 30),
            place(PLUGIN, 4),
            place(OTHER, 9),
        ];
        let anchored = anchor(
            vec![],
            vec![candidate("gone", &[(ROUTES, 9)])],
            &[],
            &[],
            &d,
            &BTreeSet::new(),
            &unread,
            &[],
        );
        assert_eq!(anchored.removed, vec!["gone"]);
        assert_eq!(
            anchored.unread_written,
            vec![place(ROUTES, 10), place(PLUGIN, 4)]
        );
    }

    #[test]
    fn the_summary_names_what_was_left_out() {
        let anchored: Anchored<&str> = Anchored {
            new: vec!["a", "b", "c"],
            removed: vec!["d"],
            left_out: LeftOut {
                edited_file: 6,
                untouched_file: 2,
                main_moved: 1,
                placed_by_file: 0,
                reading_moved: 0,
            },
            unread_written: Vec::new(),
            lost: Vec::new(),
            unread_changed: Vec::new(),
        };
        assert_eq!(
            anchored.summary(),
            "PR delta: 3 new and 1 removed operation(s) placed by this PR's diff; 9 left out \
             because the diff does not place them (6 in files this PR edits, 2 in files it does \
             not touch, 1 main's index holds at another commit)"
        );
    }

    // The git reader.

    fn files_of(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn the_reader_gives_an_entry_for_each_added_deleted_renamed_and_edited_file() {
        let (repo, head) = committed_repo(&[
            ("a.ts", "one\ntwo\nthree\n"),
            ("b.ts", "b\n"),
            ("c.ts", "c\n"),
            ("d.ts", "d\n"),
        ]);
        std::fs::write(repo.path().join("a.ts"), "one\n2\nthree\n").unwrap();
        git_in(repo.path(), &["rm", "-q", "b.ts"]);
        git_in(repo.path(), &["mv", "c.ts", "e.ts"]);
        std::fs::write(repo.path().join("f.ts"), "new\n").unwrap();
        let read = read_diff(
            &repo.path().to_string_lossy(),
            &head,
            &files_of(&["a.ts", "b.ts", "c.ts", "d.ts", "e.ts", "f.ts"]),
        )
        .unwrap();
        assert_eq!(
            read,
            diff(vec![
                ("a.ts", FileDiff::Modified(vec![hunk(2, 1, 2, 1)])),
                ("b.ts", FileDiff::Deleted),
                ("c.ts", FileDiff::Deleted),
                ("e.ts", FileDiff::Added),
                ("f.ts", FileDiff::Added),
            ])
        );
    }

    #[test]
    fn the_reader_names_paths_as_the_scan_root_does() {
        let (repo, head) = committed_repo(&[("svc/src/a.ts", "one\ntwo\n"), ("other/x.ts", "x\n")]);
        std::fs::write(repo.path().join("svc/src/a.ts"), "one\n2\n").unwrap();
        std::fs::write(repo.path().join("other/x.ts"), "y\n").unwrap();
        let root = repo.path().join("svc");
        let read = read_diff(
            &root.to_string_lossy(),
            &head,
            &files_of(&["src/a.ts", "x.ts"]),
        )
        .unwrap();
        assert_eq!(
            read,
            diff(vec![(
                "src/a.ts",
                FileDiff::Modified(vec![hunk(2, 1, 2, 1)])
            )])
        );
    }

    #[test]
    fn a_head_checkout_does_not_read_what_the_base_gained_after_the_fork() {
        let (repo, fork) = committed_repo(&[("routes.ts", "a\nb\nc\n"), ("main.ts", "m\n")]);
        let dir = repo.path();
        git_in(dir, &["checkout", "-q", "-b", "pr"]);
        std::fs::write(dir.join("routes.ts"), "a\nB\nc\n").unwrap();
        git_in(dir, &["commit", "-qam", "pr edit"]);
        git_in(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("main.ts"), "m\nmore\n").unwrap();
        std::fs::write(dir.join("gained.ts"), "g\n").unwrap();
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-qm", "main moves"]);
        let base = git_in(dir, &["rev-parse", "HEAD"]);
        git_in(dir, &["checkout", "-q", "pr"]);
        assert_ne!(base, fork);

        let anchored = run(
            &dir.to_string_lossy(),
            &base,
            vec![
                candidate("in the pr", &[("routes.ts", 2)]),
                candidate("main's", &[("gained.ts", 1)]),
            ],
            vec![],
            &[],
            vec![],
            |_| BTreeSet::new(),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(anchored.new, vec!["in the pr"]);
        assert_eq!(anchored.left_out.untouched_file, 1);
    }

    /// carrick#3222: the diff is read for a file that holds only an unread
    /// route site, so a registration the PR moved into another file is seen
    /// as written.
    #[test]
    fn the_reader_reads_a_file_that_holds_only_an_unread_route_site() {
        let (repo, base) = committed_repo(&[
            ("routes.ts", "a\nroute\nc\n"),
            ("plugin.ts", "p\nq\n"),
            ("kept.ts", "k\nl\n"),
        ]);
        let dir = repo.path();
        std::fs::write(dir.join("routes.ts"), "a\nc\n").unwrap();
        std::fs::write(dir.join("plugin.ts"), "p\nunread\n").unwrap();
        let anchored = run(
            &dir.to_string_lossy(),
            &base,
            vec![],
            vec![candidate("gone", &[("routes.ts", 2)])],
            &[],
            vec![],
            |_| BTreeSet::new(),
            &[place("plugin.ts", 2), place("kept.ts", 2)],
            &[],
        )
        .unwrap();
        assert_eq!(anchored.removed, vec!["gone"]);
        assert_eq!(anchored.unread_written, vec![place("plugin.ts", 2)]);
    }

    #[test]
    fn a_run_without_candidates_asks_git_nothing() {
        // Not a repository: any git call would be an error.
        let dir = tempfile::TempDir::new().unwrap();
        let anchored: Anchored<&str> = run(
            &dir.path().to_string_lossy(),
            &"a".repeat(40),
            vec![],
            vec![],
            &[],
            vec![],
            |_| panic!("no candidate, no question"),
            &[place("a.ts", 1)],
            &[],
        )
        .unwrap();
        assert!(anchored.new.is_empty() && anchored.removed.is_empty());
        assert!(anchored.unread_written.is_empty());
        assert!(
            run(
                &dir.path().to_string_lossy(),
                &"a".repeat(40),
                vec![candidate("x", &[("a.ts", 1)])],
                vec![],
                &[],
                vec![],
                |_| BTreeSet::new(),
                &[],
                &[],
            )
            .is_err(),
            "git cannot read a diff here"
        );
    }

    /// carrick#1712: the PR changes an input of the service other than a
    /// source file, so a row it shares a place with on main may be the same
    /// registration read another way.
    #[test]
    fn a_same_place_pair_in_a_service_whose_reading_moved_is_left_out() {
        for edit in [None, Some(FileDiff::Modified(vec![hunk(40, 1, 40, 1)]))] {
            let d = Diff(edit.into_iter().map(|e| (ROUTES.to_string(), e)).collect());
            let main = [main_row(ROUTES, 10, AtBase::At(10))];
            let scan = [place(ROUTES, 10)];
            let (new, removed, out) = listed_moved(
                vec![candidate("/new", &[(ROUTES, 10)])],
                vec![Candidate {
                    item: "/old",
                    places: vec![place(ROUTES, 10)],
                }],
                &scan,
                &main,
                &d,
                &web_moved(),
            );
            assert!(new.is_empty() && removed.is_empty(), "{d:?}");
            assert_eq!(out.reading_moved, 2, "{d:?}");
            assert_eq!(out.total(), 2);
            // The same pair lists in a service whose reading held.
            let (new, removed, _) = listed(
                vec![candidate("/new", &[(ROUTES, 10)])],
                vec![Candidate {
                    item: "/old",
                    places: vec![place(ROUTES, 10)],
                }],
                &scan,
                &main,
                &d,
            );
            assert_eq!((new, removed), (vec!["/new"], vec!["/old"]), "{d:?}");
        }
    }

    #[test]
    fn rows_the_diff_places_by_line_are_listed_in_a_service_whose_reading_moved() {
        let moved = web_moved();
        let main_at = |line| [main_row(ROUTES, line, AtBase::At(line))];
        // An added file, a changed line and a row with no line, new.
        let added = diff(vec![(ROUTES, FileDiff::Added)]);
        let edited = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(9, 1, 9, 2)]))]);
        for (rows, d) in [
            (&[(ROUTES, 4)][..], &added),
            (&[(ROUTES, 9)][..], &edited),
            (&[(ROUTES, 0)][..], &edited),
        ] {
            let (new, _, out) = listed_moved(
                vec![candidate("x", rows)],
                vec![],
                &[],
                &main_at(rows[0].1),
                d,
                &moved,
            );
            assert_eq!(new, vec!["x"], "{rows:?}");
            assert_eq!(out, LeftOut::default());
        }
        // A removed row in a deleted file and on a replaced line.
        for d in [
            diff(vec![(ROUTES, FileDiff::Deleted)]),
            diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(5, 1, 5, 2)]))]),
        ] {
            let row = main_row(ROUTES, 5, AtBase::At(5));
            let candidate = Candidate {
                item: "x",
                places: vec![row.place.clone()],
            };
            let (_, removed, _) = listed_moved(vec![], vec![candidate], &[], &[row], &d, &moved);
            assert_eq!(removed, vec!["x"], "{d:?}");
        }
    }

    #[test]
    fn a_removed_row_whose_scan_row_moved_through_the_hunks_is_left_out_when_reading_moved() {
        let d = diff(vec![(ROUTES, FileDiff::Modified(vec![hunk(0, 0, 1, 2)]))]);
        let row = main_row(ROUTES, 5, AtBase::At(5));
        let candidate = Candidate {
            item: "x",
            places: vec![row.place.clone()],
        };
        let (_, removed, out) = listed_moved(
            vec![],
            vec![candidate],
            &[place(ROUTES, 7)],
            &[row],
            &d,
            &web_moved(),
        );
        assert!(removed.is_empty());
        assert_eq!(out.reading_moved, 1);
    }

    #[test]
    fn the_summary_names_a_service_whose_dependencies_or_config_the_pr_changes() {
        let anchored: Anchored<&str> = Anchored {
            new: vec![],
            removed: vec![],
            left_out: LeftOut {
                reading_moved: 2,
                ..LeftOut::default()
            },
            unread_written: Vec::new(),
            lost: Vec::new(),
            unread_changed: Vec::new(),
        };
        assert!(
            anchored
                .summary()
                .contains("2 in a service whose dependencies or config this PR changes")
        );
    }

    #[test]
    fn mains_row_moves_from_its_commit_to_the_merge_base() {
        let (repo, base) = committed_repo(&[("routes.ts", "a\nb\nroute\nd\n"), ("k.ts", "k\n")]);
        let dir = repo.path();
        let path = dir.to_string_lossy().to_string();
        // Main moves on: two lines above the route, and the last line goes.
        std::fs::write(dir.join("routes.ts"), "x\ny\na\nb\nroute\n").unwrap();
        git_in(dir, &["commit", "-qam", "main moves"]);
        let moved = git_in(dir, &["rev-parse", "HEAD"]);
        git_in(dir, &["checkout", "-q", &base]);

        let source = |file: &str, line: u32, commit: &str| MainSource {
            place: place(file, line),
            commit: commit.to_string(),
        };
        let placed = place_main_rows(
            &path,
            &base,
            vec![
                source("routes.ts", 5, &moved),
                source("routes.ts", 1, &moved),
                source("k.ts", 1, &moved),
                source("routes.ts", 3, &base),
                source("routes.ts", 5, &"f".repeat(40)),
            ],
        );
        let at: Vec<AtBase> = placed.iter().map(|row| row.at_base).collect();
        assert_eq!(
            at,
            vec![
                AtBase::At(3),
                AtBase::Moved,
                AtBase::At(1),
                AtBase::At(3),
                AtBase::Unreadable,
            ]
        );
        let stored: Vec<bool> = placed.iter().map(|row| row.stored_at_base).collect();
        assert_eq!(stored, vec![false, false, false, true, false]);
    }
}
