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
}

/// Why a candidate was left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    EditedFile,
    UntouchedFile,
    MainMoved,
    PlacedByFile,
}

impl Reason {
    fn label(self) -> &'static str {
        match self {
            Reason::EditedFile => "edited_file",
            Reason::UntouchedFile => "untouched_file",
            Reason::MainMoved => "main_moved",
            Reason::PlacedByFile => "placed_by_file",
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
}

impl LeftOut {
    pub fn total(&self) -> usize {
        self.edited_file + self.untouched_file + self.main_moved + self.placed_by_file
    }

    fn count(&mut self, reason: Reason) {
        match reason {
            Reason::EditedFile => self.edited_file += 1,
            Reason::UntouchedFile => self.untouched_file += 1,
            Reason::MainMoved => self.main_moved += 1,
            Reason::PlacedByFile => self.placed_by_file += 1,
        }
    }
}

/// The candidates the PR's diff places, and what was left out.
#[derive(Debug)]
pub struct Anchored<T> {
    pub new: Vec<T>,
    pub removed: Vec<T>,
    pub left_out: LeftOut,
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

    /// Line `line` of `file` at the merge base, in the working tree.
    fn moved_through_diff(&self, file: &str, line: u32) -> Option<u32> {
        match self.diff.0.get(file) {
            None => Some(line),
            Some(FileDiff::Modified(hunks)) => map_line(hunks, line),
            Some(FileDiff::Added | FileDiff::Deleted) => None,
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
            return Ok(());
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
                    Ok(())
                }
                Some(_) => Err(Reason::EditedFile),
            },
            None if self.scan_holds(&at_base) => Ok(()),
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
/// candidate.
pub fn anchor<T>(
    new: Vec<Candidate<T>>,
    removed: Vec<Candidate<T>>,
    scan: &[Place],
    main: &[MainRow],
    diff: &Diff,
) -> Anchored<T> {
    let context = Context { diff, main, scan };
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
            Ok(()) => listed_removed.push(candidate.item),
            Err(reason) => {
                debug!(
                    "PR delta: removed operation at {:?} left out ({})",
                    candidate.places,
                    reason.label()
                );
                left_out.count(reason);
            }
        }
    }
    Anchored {
        new: listed_new,
        removed: listed_removed,
        left_out,
    }
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
            MainRow { place, at_base }
        })
        .collect()
}

/// [`anchor`] over the diff git gives for the checkout at `repo_path`.
///
/// The diff runs from the merge base of `pr_base` and HEAD to the working
/// tree: on a merge checkout the merge base is `pr_base`, and on a head
/// checkout it is the fork point, so commits main gained after the PR
/// branched are never read as the PR's (as `pr_functions::block_for_run`
/// reads it). Git is asked only about files that hold a candidate row; a run
/// with no candidate makes no git call. `Err` means git could not read the
/// diff.
pub fn run<T>(
    repo_path: &str,
    pr_base: &str,
    new: Vec<Candidate<T>>,
    removed: Vec<Candidate<T>>,
    scan: &[Place],
    main: Vec<MainSource>,
) -> Result<Anchored<T>, String> {
    if new.is_empty() && removed.is_empty() {
        return Ok(anchor(new, removed, scan, &[], &Diff::default()));
    }
    let files: BTreeSet<String> = new
        .iter()
        .chain(&removed)
        .flat_map(|candidate| candidate.places.iter().map(|place| place.file.clone()))
        .collect();
    let head = crate::git_state::head_commit(repo_path)?;
    let merge_base = crate::git_state::merge_base(repo_path, pr_base, &head)?;
    let diff = read_diff(repo_path, &merge_base, &files)?;
    let in_files: Vec<MainSource> = main
        .into_iter()
        .filter(|row| files.contains(&row.place.file))
        .collect();
    let main = place_main_rows(repo_path, &merge_base, in_files);
    Ok(anchor(new, removed, scan, &main, &diff))
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
        let anchored = anchor(new, removed, scan, main, diff);
        (anchored.new, anchored.removed, anchored.left_out)
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
            },
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
        )
        .unwrap();
        assert_eq!(anchored.new, vec!["in the pr"]);
        assert_eq!(anchored.left_out.untouched_file, 1);
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
        )
        .unwrap();
        assert!(anchored.new.is_empty() && anchored.removed.is_empty());
        assert!(
            run(
                &dir.path().to_string_lossy(),
                &"a".repeat(40),
                vec![candidate("x", &[("a.ts", 1)])],
                vec![],
                &[],
                vec![],
            )
            .is_err(),
            "git cannot read a diff here"
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
    }
}
