//! Per-position compiler rows (carrick#2405, move 2 of the fact layer).
//!
//! Before a service's analysis starts its sidecar, a fresh record-only
//! sidecar walks the service's source files and writes, for every call,
//! member access, comparison and expression identifier, what the compiler
//! knows there: the symbol, the declaration's file and line, the declaring
//! package and the type text. The walk is the sidecar's
//! (`src/sidecar/src/position-rows.ts`); the walks are admitted beside the
//! analyses, on the run's one memory budget (`crate::engine::walk_lane`,
//! carrick#3085), and a service's walk process exits before its own analysis
//! starts.
//!
//! The walk runs only with [`POSITION_ROWS_ENV`] set to `1`. With a local
//! store directory ([`CACHE_DIR_ENV`]) it writes
//! `<store>/positions/<repo>__<service>.jsonl.gz` beside the blobs. Both blob
//! listings read top-level `*.json` only, so the subdirectory is never read
//! as a blob. Without one (the Action, the cloud runner) it writes under a
//! per-run directory in the OS temp dir, never inside the scanned tree, and
//! the file is removed when the run ends ([`Destination`]). A walk that dies or
//! misses its deadline loses that service's rows and says so in the run log,
//! and nothing else: it is never a scan-health entry, a finding or a stop.
//!
//! The rows are read before each analysis pass's discovery (the loop, and
//! the owed-work retry), into a [`PositionIndex`] the
//! request summaries carry. Nothing reads a row yet (move 3 of the fact
//! layer reads them slice by slice); a lookup that finds none is a hole with
//! a reason ([`HoleReason`]), never a fallback.
//!
//! The file format is pinned here by serde types both sides read
//! (`tests/fixtures/position-rows/api.expected.jsonl`). seams-engineer owns it
//! from move 3; a change to it is a seam change.
//!
//! The file is gzip JSON lines. Line 1 is the [`Header`]. Then one
//! [`FileRows`] line per source file, in request order. A row's key is
//! `(start, end, kind)`: TypeScript `getStart()`/`getEnd()` of the node, in
//! UTF-16 code units from the start of the file, which is what
//! [`crate::utils::sidecar_position`] gives for an SWC span. [`Kind::Decl`]
//! rows are the kind several rows share a key of
//! ([`PositionIndex::union_declarations`]).
//!
//! Version 2 (carrick#2430) added `impl` rows and [`Kind::Jsx`]; version 3
//! (carrick#2448) added [`Kind::Decl`]; version 4 (carrick#2561) dropped
//! `impl` rows, whose go-to-implementation search was most of the walk and
//! which nothing read (carrick#2432 asks for implementations where discovery
//! stops). A file of any other version is not
//! read: its service reads as [`HoleReason::RowsMissing`]. A walk removes any
//! earlier rows file before it writes. The rows of a service a later laptop
//! pass carries are used again instead of walked, only when [`kept_rows`]
//! finds them exactly what a walk would write (carrick#3027).
//!
//! [`FLAG_PUBLISHED`] and [`FLAG_MODULE_DEFAULT`] (carrick#2508) are flag
//! bits, not a new version: a row written without them
//! still reads, and reads as a declaration nothing says is published, which
//! discovery's name match abstains on. [`FLAG_RUNS_HANDED`] (carrick#2634)
//! is one too: a call row without it reads as "not stated", which abstains.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::utils::Utf16Offsets;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::cloud_storage::{CACHE_DIR_ENV, blob_stem};
use crate::request_summary::HoleReason;
use crate::services::type_sidecar::{PositionRowsCounts, SidecarRecipe};

/// Set to `1` to walk each service and read its rows.
pub const POSITION_ROWS_ENV: &str = "CARRICK_POSITION_ROWS";

/// The store subdirectory the rows files go in.
pub const POSITIONS_DIR: &str = "positions";

/// The format version the header carries.
pub const FORMAT_VERSION: u32 = 4;

/// The type is `any`, declared or inferred. Never set with [`FLAG_ERROR_TYPE`].
pub const FLAG_ANY: u8 = 1;
/// The type is the compiler's error type: something did not resolve.
pub const FLAG_ERROR_TYPE: u8 = 2;
/// The checker threw at this node; the row holds nothing else.
pub const FLAG_CHECKER_THREW: u8 = 4;
/// The declaration is outside the checkout, not the default library and in
/// no installed package, so `decl_file` is `null`.
pub const FLAG_DECLARATION_UNNAMED: u8 = 8;
/// A [`Kind::Decl`] row standing for more rows than the walk lists (16); it
/// names none.
pub const FLAG_TOO_MANY: u8 = 16;
/// The declaration is published under the row's symbol name: its module
/// exports it by that name, or it is a global (a script's top-level
/// declaration, or one in `declare global`). A member of a class,
/// interface, type literal or object literal never is.
pub const FLAG_PUBLISHED: u8 = 32;
/// The declaration is what its module exports as `export =` or `default`.
pub const FLAG_MODULE_DEFAULT: u8 = 64;
/// A [`Kind::Call`] row only: the call's declared return is the result of
/// the function it is handed (`ReturnType<F>`, a type parameter the handed
/// function returns, or an awaitable of one), so the library runs it and
/// hands its result back (carrick#2634). The last free bit of the `u8`: the
/// next flag widens the field and bumps the format. Pinned to the sidecar's
/// `FLAG_RUNS_HANDED` by `position-rows-runs-handed.test.ts`.
pub const FLAG_RUNS_HANDED: u8 = 128;

/// Line 1 of a rows file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub v: u32,
    /// The compiler version the rows were read with.
    pub typescript: String,
    pub service: String,
    /// How many [`FileRows`] lines follow.
    pub files: usize,
}

/// One source file's rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRows {
    /// Relative to the scanned repo's root, with `/` separators.
    pub f: String,
    /// sha256 hex of the file's bytes on disk, BOM included. Absent only for
    /// a file that could not be read. A reader whose bytes hash otherwise
    /// holds stale rows, and that is a hole, never a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<String>,
    /// Why the file has no rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<Skip>,
    pub r: Vec<Row>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    Unreadable,
    NotInProgram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A call, `new` or tagged template: the callee's symbol, the result's type.
    Call,
    /// A property or element access: the name's or argument's symbol.
    Member,
    /// `=== !== == != < > <= >= instanceof in`: the left operand.
    Compare,
    /// An identifier in expression position.
    Ident,
    /// One declaration of a `call` or `member` row's member read on a union
    /// whose constituents declare it at more than one place (two classes
    /// behind `a ? x : y`), at that row's span, which names only the first:
    /// the constituent member's declaration, or for a member with none (a
    /// mapped type's) the constituent type's own; no type. Ordered by
    /// declaring file, then position; past the cap, one row with
    /// [`FLAG_TOO_MANY`] and nothing else.
    Decl,
    /// A JSX opening or self-closing element, at its tag: the declaration
    /// the tag names (for an intrinsic, the `JSX.IntrinsicElements` member)
    /// and the tag's type (an intrinsic's attributes).
    Jsx,
}

/// `[kind, start, end, symbol, decl_file, decl_line, decl_package, type_text, flags]`.
///
/// `decl_file` is a repo-relative path (`node_modules/...` included),
/// `lib:<file>` for the compiler's default library, `pkg:<name>/<path>` for
/// an installed package outside the repo root, or `null` with
/// [`FLAG_DECLARATION_UNNAMED`]. `decl_line` is 1-based.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row(
    pub Kind,
    pub u32,
    pub u32,
    pub Option<String>,
    pub Option<String>,
    pub Option<u32>,
    pub Option<String>,
    pub Option<String>,
    pub u8,
);

impl Row {
    pub fn kind(&self) -> Kind {
        self.0
    }
    pub fn start(&self) -> u32 {
        self.1
    }
    pub fn end(&self) -> u32 {
        self.2
    }
    pub fn symbol(&self) -> Option<&str> {
        self.3.as_deref()
    }
    pub fn decl_file(&self) -> Option<&str> {
        self.4.as_deref()
    }
    pub fn decl_line(&self) -> Option<u32> {
        self.5
    }
    pub fn decl_package(&self) -> Option<&str> {
        self.6.as_deref()
    }
    pub fn type_text(&self) -> Option<&str> {
        self.7.as_deref()
    }
    pub fn flags(&self) -> u8 {
        self.8
    }
}

/// A rows file, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowsFile {
    pub header: Header,
    pub files: Vec<FileRows>,
}

/// Read a rows file from gzip JSON lines.
pub fn read_rows_file(path: &Path) -> Result<RowsFile, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    read_rows(BufReader::new(flate2::read::GzDecoder::new(file)))
}

/// [`read_rows_file`] over decompressed lines.
pub fn read_rows(reader: impl BufRead) -> Result<RowsFile, String> {
    let mut lines = reader.lines();
    let header: Header = match lines.next() {
        Some(line) => serde_json::from_str(&line.map_err(|e| e.to_string())?)
            .map_err(|e| format!("header: {e}"))?,
        None => return Err("an empty rows file".to_string()),
    };
    if header.v != FORMAT_VERSION {
        return Err(format!(
            "format version {} is not {FORMAT_VERSION}",
            header.v
        ));
    }
    let mut files = Vec::with_capacity(header.files);
    for (index, line) in lines.enumerate() {
        let line = line.map_err(|e| e.to_string())?;
        if line.is_empty() {
            continue;
        }
        files.push(serde_json::from_str(&line).map_err(|e| format!("line {}: {e}", index + 2))?);
    }
    if files.len() != header.files {
        return Err(format!(
            "the header says {} file(s) and {} follow",
            header.files,
            files.len()
        ));
    }
    Ok(RowsFile { header, files })
}

/// The digest a rows file's `h` holds for `bytes`: sha256, lowercase hex.
pub fn content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Where one service's rows file goes this run.
///
/// Beside the blobs when there is a local store, and kept there. Otherwise
/// under a directory of this run's own in the OS temp dir: the file and, once
/// empty, the directory are removed when this is dropped, which is when the
/// run ends (carrick#2413).
#[derive(Debug)]
pub struct Destination {
    path: PathBuf,
    /// The per-run temp directory the file sits in, when there is no store.
    temporary: Option<PathBuf>,
}

impl Destination {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rows an earlier pass kept at `path`: never removed by this run.
    pub fn kept(path: PathBuf) -> Self {
        Self {
            path,
            temporary: None,
        }
    }

    /// Whether the file is removed when this is dropped.
    pub fn is_temporary(&self) -> bool {
        self.temporary.is_some()
    }
}

impl Drop for Destination {
    fn drop(&mut self) {
        if let Some(dir) = &self.temporary {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(partial_path(&self.path));
            // Another service of this run may still be writing beside it.
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Where a service's rows go this run, or `None` when the flag is unset.
pub fn destination(repo_name: &str, service_name: Option<&str>) -> Option<Destination> {
    destination_from(
        std::env::var(POSITION_ROWS_ENV).ok().as_deref(),
        std::env::var_os(CACHE_DIR_ENV)
            .map(PathBuf::from)
            .as_deref(),
        &std::env::temp_dir(),
        repo_name,
        service_name,
    )
}

/// [`destination`] over the settings it reads: the flag, the local store and
/// the OS temp dir.
pub fn destination_from(
    flag: Option<&str>,
    store: Option<&Path>,
    temp: &Path,
    repo_name: &str,
    service_name: Option<&str>,
) -> Option<Destination> {
    if flag != Some("1") {
        return None;
    }
    let name = format!("{}.jsonl.gz", blob_stem(repo_name, service_name));
    Some(match store.filter(|dir| !dir.as_os_str().is_empty()) {
        Some(store) => Destination {
            path: store.join(POSITIONS_DIR).join(name),
            temporary: None,
        },
        None => {
            let dir = temp.join(format!("carrick-positions-{}", std::process::id()));
            Destination {
                path: dir.join(name),
                temporary: Some(dir),
            }
        }
    })
}

/// The directory a laptop pass keeps its rows in between passes
/// (carrick#3027). Set by the indexer (`refresh`, `index`, `init`) on its
/// scans and, read-only, on `check --changed`'s; without it the rows sit in
/// the store's `positions/` directory, which outlives a run only when the
/// store does.
pub const KEPT_DIR_ENV: &str = "CARRICK_KEPT_POSITIONS_DIR";

/// Where an earlier pass may have left `service`'s rows: [`KEPT_DIR_ENV`],
/// else the local store's `positions/` directory. `None` with neither (the
/// Action, the cloud runner): their rows are per run and nothing is kept.
pub fn kept_path(repo_name: &str, service_name: Option<&str>) -> Option<PathBuf> {
    kept_path_from(
        std::env::var_os(KEPT_DIR_ENV).map(PathBuf::from).as_deref(),
        std::env::var_os(CACHE_DIR_ENV)
            .map(PathBuf::from)
            .as_deref(),
        repo_name,
        service_name,
    )
}

/// [`kept_path`] over the settings it reads.
pub fn kept_path_from(
    kept: Option<&Path>,
    store: Option<&Path>,
    repo_name: &str,
    service_name: Option<&str>,
) -> Option<PathBuf> {
    let present = |dir: &&Path| !dir.as_os_str().is_empty();
    let dir = kept
        .filter(present)
        .map(Path::to_path_buf)
        .or_else(|| store.filter(present).map(|s| s.join(POSITIONS_DIR)))?;
    Some(dir.join(format!("{}.jsonl.gz", blob_stem(repo_name, service_name))))
}

/// Which build wrote a rows file: the walk's own code is part of this binary.
fn walk_stamp() -> String {
    let build = match crate::cloud_storage::ScannerBuild::current() {
        Some(build) => format!(
            "{}{}",
            build.commit,
            if build.dirty { "+dirty" } else { "" }
        ),
        None => "unstamped".to_string(),
    };
    format!("{} {build}", env!("CARGO_PKG_VERSION"))
}

/// The record beside a rows file of what its walk was a walk of.
pub fn walked_at_path(rows: &Path) -> PathBuf {
    let mut name = rows.as_os_str().to_owned();
    name.push(".at");
    PathBuf::from(name)
}

/// Record that the rows file at `rows` were walked from the inputs a service
/// has at `commit` (this build's stamp beside it), or, with `None` (a tree
/// that is not a clean checkout of a commit), that nothing vouches for them.
/// Any earlier record is removed first.
pub fn record_walked_at(rows: &Path, commit: Option<&str>) {
    let record = walked_at_path(rows);
    let _ = std::fs::remove_file(&record);
    if let Some(commit) = commit
        && let Err(error) = std::fs::write(&record, format!("{}\n{commit}\n", walk_stamp()))
    {
        debug!(
            "The walk record {} was not written: {error}",
            record.display()
        );
    }
}

/// Why kept rows are not used, and the service is walked.
#[derive(Debug, PartialEq, Eq)]
pub enum Miss {
    /// No rows file, or no record of what it was walked from.
    NotKept,
    /// Walked by another build of the scanner.
    OtherBuild,
    /// Walked from the inputs of another commit than the one this run carries.
    OtherCommit,
    /// The rows file could not be read.
    Unreadable(String),
    /// The rows name other files than the service has now.
    FilesDiffer,
    /// A file's bytes hash otherwise than its rows, or cannot be read.
    FileChanged(String),
}

/// Whether the rows file at `rows` is exactly what a walk of `files` (the
/// service's source files, under `scan_root`) would write now, given that
/// the service is carried at `commit`: it was walked by this build from the
/// inputs of that commit, names the same files, and every file's bytes hash
/// as its rows say (BOM included, as [`PositionIndex::bind`] hashes them).
/// Any other answer walks, as before (carrick#3027).
pub fn kept_rows(
    rows: &Path,
    commit: &str,
    scan_root: &Path,
    files: &[PathBuf],
) -> Result<(), Miss> {
    let record = std::fs::read_to_string(walked_at_path(rows)).map_err(|_| Miss::NotKept)?;
    let mut lines = record.lines();
    if lines.next() != Some(walk_stamp().as_str()) {
        return Err(Miss::OtherBuild);
    }
    if lines.next() != Some(commit) {
        return Err(Miss::OtherCommit);
    }
    let kept = read_rows_file(rows).map_err(Miss::Unreadable)?;
    kept_rows_match(&kept, scan_root, files)
}

/// [`kept_rows`]' check of the rows against the files on disk.
pub fn kept_rows_match(rows: &RowsFile, scan_root: &Path, files: &[PathBuf]) -> Result<(), Miss> {
    let named: BTreeSet<&str> = rows.files.iter().map(|file| file.f.as_str()).collect();
    let wanted: BTreeSet<String> = files
        .iter()
        .filter_map(|file| {
            let relative = file.strip_prefix(scan_root).ok()?;
            Some(
                relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        })
        .collect();
    if wanted.len() != files.len()
        || named.len() != rows.files.len()
        || named != wanted.iter().map(String::as_str).collect()
    {
        return Err(Miss::FilesDiffer);
    }
    for file in &rows.files {
        let current = std::fs::read(scan_root.join(&file.f))
            .map(|bytes| content_hash(&bytes))
            .ok();
        if file.skip == Some(Skip::Unreadable) || file.h.is_none() || file.h != current {
            return Err(Miss::FileChanged(file.f.clone()));
        }
    }
    Ok(())
}

/// A service's rows, read once before discovery for the request summaries
/// to look up by span (carrick#2413).
///
/// Every file is bound to the bytes discovery read ([`Self::bind`]) before a
/// lookup in it can succeed: a file whose bytes hash otherwise than its rows'
/// `h` holds stale rows. A lookup that finds no usable row says why, with a
/// [`HoleReason`]; it never guesses.
#[derive(Debug, Default)]
pub struct PositionIndex {
    /// The scanned repo's canonical root: every row path is relative to it.
    scan_root: PathBuf,
    /// By repo-relative path with `/` separators, as [`FileRows::f`].
    files: HashMap<String, IndexedFile>,
    /// The compiler version the rows were read with ([`Header::typescript`]);
    /// `None` for an index with no rows.
    typescript: Option<String>,
    /// The globals the code uses that the compiler's default library
    /// declares, by the library file (`lib.dom.d.ts`): the symbol of each
    /// identifier row whose declaration is a `lib:` file.
    library_globals: BTreeMap<String, BTreeSet<String>>,
    /// How many lookups converted a span, for the tests that a reader asks
    /// once per position.
    #[cfg(test)]
    lookups: std::sync::atomic::AtomicUsize,
}

#[derive(Debug)]
struct IndexedFile {
    hash: Option<String>,
    skip: Option<Skip>,
    rows: HashMap<(u32, u32, Kind), Row>,
    /// The [`Kind::Decl`] rows by span, in the order written.
    decls: HashMap<(u32, u32), Vec<Row>>,
    bound: Bound,
}

/// What discovery's read of a file said about its rows.
#[derive(Debug)]
enum Bound {
    /// Discovery has not read the file.
    Unread,
    /// The bytes discovery read hash to `h`. The text's offset table is kept
    /// only when its SWC byte offsets and the rows' UTF-16 offsets differ:
    /// anything but plain ASCII.
    Fresh(Option<Utf16Offsets>),
    /// The bytes discovery read hash otherwise.
    Stale,
}

impl PositionIndex {
    /// An index with no rows, for a service whose walk wrote none: every
    /// lookup is [`HoleReason::RowsMissing`].
    pub fn missing(scan_root: &Path) -> Self {
        Self {
            scan_root: scan_root.to_path_buf(),
            ..Self::default()
        }
    }

    /// The index over a rows file read for the repo rooted at `scan_root`.
    pub fn of(scan_root: &Path, rows: RowsFile) -> Self {
        let mut library_globals: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let files = rows
            .files
            .into_iter()
            .map(|file| {
                let mut rows = HashMap::new();
                let mut decls: HashMap<(u32, u32), Vec<Row>> = HashMap::new();
                for row in file.r {
                    if row.kind() == Kind::Ident
                        && let (Some(symbol), Some(library)) = (
                            row.symbol(),
                            row.decl_file().and_then(|file| file.strip_prefix("lib:")),
                        )
                    {
                        library_globals
                            .entry(library.to_string())
                            .or_default()
                            .insert(symbol.to_string());
                    }
                    match row.kind() {
                        Kind::Decl => decls.entry((row.start(), row.end())).or_default().push(row),
                        kind => {
                            rows.insert((row.start(), row.end(), kind), row);
                        }
                    }
                }
                (
                    file.f,
                    IndexedFile {
                        hash: file.h,
                        skip: file.skip,
                        rows,
                        decls,
                        bound: Bound::Unread,
                    },
                )
            })
            .collect();
        Self {
            files,
            typescript: Some(rows.header.typescript),
            library_globals,
            ..Self::missing(scan_root)
        }
    }

    /// The compiler version the rows were read with, and the globals the
    /// code uses from each default-library file the program loads
    /// (`lib.dom.d.ts` and its names): what the library store is asked
    /// about the language's own library (carrick#2504). `None` for an index
    /// with no rows.
    pub fn default_library_globals(&self) -> Option<(&str, &BTreeMap<String, BTreeSet<String>>)> {
        Some((self.typescript.as_deref()?, &self.library_globals))
    }

    /// How many lookups converted a span so far.
    #[cfg(test)]
    pub(crate) fn lookups(&self) -> usize {
        self.lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Every `call` row of every file, in no fixed order: the members the
    /// code calls, for the receiver trace's declaration ask (carrick#2474).
    pub fn call_rows(&self) -> impl Iterator<Item = &Row> {
        self.files
            .values()
            .flat_map(|file| file.rows.values())
            .filter(|row| row.kind() == Kind::Call)
    }

    /// Whether any call the rows hold may go through an import that did not
    /// resolve: a `call` row of the error type that names a symbol declared
    /// in the call's own file. That is the row an unresolved import's call
    /// reads as (`RequestSummaryInputs::row_names`), whose callee the scan
    /// then reads from the call graph instead (carrick#2557), into any
    /// service. Without the file's IR the row cannot tell an import from a
    /// definition, so a call to a definition of the same file whose type
    /// did not resolve answers yes too.
    pub fn may_call_through_an_unresolved_import(&self) -> bool {
        self.files.iter().any(|(file, indexed)| {
            indexed.rows.values().any(|row| {
                row.kind() == Kind::Call
                    && row.flags() & FLAG_ERROR_TYPE != 0
                    && row.symbol().is_some()
                    && row.decl_file() == Some(file.as_str())
            })
        })
    }

    /// How many files the rows name.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// How many of the files the rows name are in each state: `fresh`,
    /// `stale`, `unread` (discovery did not read it), `not_in_program` and
    /// `unreadable` (the walk's skips). For the run log and the census.
    pub fn file_states(&self) -> std::collections::BTreeMap<&'static str, usize> {
        let mut states = std::collections::BTreeMap::new();
        for file in self.files.values() {
            let state = match (&file.skip, &file.bound) {
                (Some(Skip::NotInProgram), _) => "not_in_program",
                (Some(Skip::Unreadable), _) => "unreadable",
                (None, Bound::Fresh(_)) => "fresh",
                (None, Bound::Stale) => "stale",
                (None, Bound::Unread) => "unread",
            };
            *states.entry(state).or_default() += 1;
        }
        states
    }

    /// `file` (a path under the scan root) as the rows name it.
    fn relative(&self, file: &Path) -> Option<String> {
        let relative = file.strip_prefix(&self.scan_root).ok()?;
        let parts: Vec<_> = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect();
        Some(parts.join("/"))
    }

    /// Record the text discovery parsed `file` from, as read from disk (BOM
    /// included): the rows' `h` is checked against it, and lookups in the
    /// file convert spans over it.
    pub fn bind(&mut self, file: &Path, text: &str) {
        let Some(relative) = self.relative(file) else {
            return;
        };
        let Some(indexed) = self.files.get_mut(&relative) else {
            return;
        };
        indexed.bound = if indexed.hash.as_deref() == Some(content_hash(text.as_bytes()).as_str()) {
            Bound::Fresh((!text.is_ascii()).then(|| Utf16Offsets::new(text)))
        } else {
            Bound::Stale
        };
    }

    /// Why `file` holds no usable rows, or its rows.
    fn file(&self, file: &Path) -> Result<(&IndexedFile, Option<&Utf16Offsets>), HoleReason> {
        let indexed = self
            .relative(file)
            .and_then(|relative| self.files.get(&relative))
            .ok_or(HoleReason::RowsMissing)?;
        if indexed.skip.is_some() {
            return Err(HoleReason::NotInProgram);
        }
        match &indexed.bound {
            Bound::Fresh(text) => Ok((indexed, text.as_ref())),
            Bound::Stale => Err(HoleReason::RowsStale),
            Bound::Unread => Err(HoleReason::RowsMissing),
        }
    }

    /// The row of `kind` at the SWC span `lo..hi` of `file`, counted from
    /// [`crate::swc_scanner::SWC_SPAN_BASE`] as in a file parsed on its own
    /// (what [`crate::request_summary::FileIr`] spans convert to).
    pub fn lookup(&self, file: &Path, lo: u32, hi: u32, kind: Kind) -> Result<&Row, HoleReason> {
        let (indexed, text) = self.file(file)?;
        #[cfg(test)]
        self.lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (start, end) = rows_span(text, lo, hi);
        let row = indexed
            .rows
            .get(&(start, end, kind))
            .ok_or(HoleReason::RowsMissing)?;
        if row.flags() & FLAG_CHECKER_THREW != 0 {
            return Err(HoleReason::CheckerThrew);
        }
        Ok(row)
    }

    /// The [`Kind::Decl`] rows at [`Self::lookup`]'s position, in the order
    /// written (by declaring file, then position). None unless the member is
    /// read on a union whose constituents declare it at more than one place;
    /// one with [`FLAG_TOO_MANY`] or [`FLAG_CHECKER_THREW`] where it names
    /// none.
    pub fn union_declarations(&self, file: &Path, lo: u32, hi: u32) -> Result<&[Row], HoleReason> {
        let (indexed, text) = self.file(file)?;
        let (start, end) = rows_span(text, lo, hi);
        Ok(indexed
            .decls
            .get(&(start, end))
            .map(Vec::as_slice)
            .unwrap_or_default())
    }

    /// The row at [`Self::lookup`]'s position whose symbol names a
    /// declaration: one with no symbol, or with the error type, is a hole
    /// that says whether the name was an import (`import_local`); a member
    /// or call row typed `any` with no symbol is an untyped receiver.
    ///
    /// A row whose symbol is declared in another file than `file` names that
    /// declaration whatever its type (carrick#2413 slice B): the error type
    /// then says only that the value's type did not resolve, as when a
    /// workspace package's own imports do not. One whose declaration is in
    /// `file` with the error type is the import itself, which reached
    /// nothing.
    pub fn declaration(
        &self,
        file: &Path,
        lo: u32,
        hi: u32,
        kind: Kind,
        import_local: bool,
    ) -> Result<&Row, HoleReason> {
        let row = self.lookup(file, lo, hi, kind)?;
        let unresolved = if import_local {
            HoleReason::UnresolvedImport
        } else {
            HoleReason::UnresolvedName
        };
        if row.flags() & FLAG_ERROR_TYPE != 0 && !self.declared_elsewhere(file, row) {
            return Err(unresolved);
        }
        if row.symbol().is_none() {
            if matches!(kind, Kind::Member | Kind::Call) && row.flags() & FLAG_ANY != 0 {
                return Err(HoleReason::UntypedReceiver);
            }
            return Err(unresolved);
        }
        Ok(row)
    }

    /// Whether `row`, looked up in `file`, names a symbol declared in another
    /// file.
    fn declared_elsewhere(&self, file: &Path, row: &Row) -> bool {
        row.symbol().is_some()
            && row
                .decl_file()
                .is_some_and(|declared| self.relative(file).as_deref() != Some(declared))
    }

    /// The file a row's repo-relative `decl_file` names, under the scan root,
    /// as discovery walks it.
    pub fn path_of(&self, decl_file: &str) -> PathBuf {
        decl_file
            .split('/')
            .fold(self.scan_root.clone(), |path, part| path.join(part))
    }

    /// Why `file` holds no usable rows, if it holds none: absent from the
    /// rows, not read by discovery, stale, or skipped by the walk.
    pub fn file_hole(&self, file: &Path) -> Option<HoleReason> {
        self.file(file).err()
    }
}

/// An SWC span of a file as the rows key it: converted by the bound text's
/// offset table when it is not plain ASCII, else less the SWC span base.
fn rows_span(text: Option<&Utf16Offsets>, lo: u32, hi: u32) -> (u32, u32) {
    match text {
        Some(text) => (text.position(lo), text.position(hi)),
        None => (
            lo.saturating_sub(crate::swc_scanner::SWC_SPAN_BASE),
            hi.saturating_sub(crate::swc_scanner::SWC_SPAN_BASE),
        ),
    }
}

/// Read the rows `destination` holds into an index for the repo rooted at
/// `scan_root`, or an empty one, with the reason logged, when there are none
/// to read. The file is read once: a temporary one is removed when
/// `destination` is dropped.
pub fn load_index(destination: &Destination, scan_root: &Path, service: &str) -> PositionIndex {
    match read_rows_file(destination.path()) {
        Ok(rows) => {
            let index = PositionIndex::of(scan_root, rows);
            debug!(
                "Position rows for service {service} read: {} file(s)",
                index.file_count()
            );
            index
        }
        Err(error) => {
            debug!("Position rows for service {service} were not read: {error}");
            PositionIndex::missing(scan_root)
        }
    }
}

/// The temporary name the sidecar writes a rows file under before it renames
/// it into place.
pub fn partial_path(out: &Path) -> PathBuf {
    let mut name = out.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// One service's walk: where it ran and what it cost.
#[derive(Debug, Clone)]
pub struct ServiceWalk<'a> {
    /// The service's root and tsconfig, as its analysis sidecar was scoped.
    pub service_root: &'a Path,
    pub tsconfig: Option<&'a str>,
    /// The scanned repo's root: every path in the rows is relative to it.
    pub scan_root: &'a Path,
    /// The service's source files, as discovery walks them.
    pub files: &'a [PathBuf],
    pub out: &'a Path,
    pub service: &'a str,
}

/// Walk one service in a fresh record-only sidecar from `recipe` and write
/// its rows to `walk.out`. The process is started here and gone when this
/// returns, so it never overlaps the service's analysis sidecar.
///
/// Any rows file from an earlier run is removed first, so a walk that fails
/// leaves none: lost rows are lost, never stale. (A pass that carries a
/// service may instead use rows an earlier pass kept, when
/// [`kept_rows`] vouches for them: carrick#3027.) Every failure is the
/// caller's to log; nothing is recorded for the run.
pub fn record_service_rows(
    recipe: &SidecarRecipe,
    walk: &ServiceWalk<'_>,
) -> Result<(PositionRowsCounts, Option<u64>), String> {
    let partial = partial_path(walk.out);
    let record = walked_at_path(walk.out);
    for stale in [walk.out, partial.as_path(), record.as_path()] {
        match std::fs::remove_file(stale) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{} could not be removed: {e}", stale.display())),
        }
    }
    let started = Instant::now();
    let answer = recipe
        .spawn_scoped_record_only(walk.service_root, walk.tsconfig)
        .and_then(|sidecar| {
            let counts = sidecar.position_rows(walk.files, walk.scan_root, walk.out, walk.service);
            let peak = sidecar.peak_resident_bytes();
            drop(sidecar);
            counts.map(|counts| (counts, peak))
        });
    match answer {
        Ok((counts, peak)) => {
            debug!(
                "Position rows for service {}: {} row(s) over {} file(s), {} byte(s) \
                 ({} resolved, {} any, {} error type, {} jsx); load {} ms, walk {} ms, \
                 wall {:.1}s, peak {}",
                walk.service,
                counts.rows,
                counts.files,
                counts.bytes,
                counts.resolved,
                counts.any,
                counts.error_type,
                counts.jsx,
                counts.load_ms,
                counts.walk_ms,
                started.elapsed().as_secs_f64(),
                peak.map(|bytes| format!("{} MB", bytes / (1024 * 1024)))
                    .unwrap_or_else(|| "unknown".to_string()),
            );
            Ok((counts, peak.map(|bytes| bytes / (1024 * 1024))))
        }
        Err(error) => {
            // The sidecar renames into place only once the file is whole; a
            // walk that died may have left its temporary file.
            let _ = std::fs::remove_file(&partial);
            let _ = std::fs::remove_file(walk.out);
            Err(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_of(destination: Option<Destination>) -> Option<(PathBuf, bool)> {
        destination.map(|d| (d.path().to_path_buf(), d.is_temporary()))
    }

    #[test]
    fn the_walk_is_off_without_the_flag() {
        let (store, temp) = (Path::new("/store"), Path::new("/tmp"));
        assert_eq!(
            path_of(destination_from(
                None,
                Some(store),
                temp,
                "repo",
                Some("api")
            )),
            None
        );
        assert_eq!(
            path_of(destination_from(
                Some("0"),
                Some(store),
                temp,
                "repo",
                Some("api")
            )),
            None
        );
    }

    #[test]
    fn the_rows_file_sits_beside_the_blobs_under_the_blob_stem() {
        let temp = Path::new("/tmp");
        assert_eq!(
            path_of(destination_from(
                Some("1"),
                Some(Path::new("/store")),
                temp,
                "repo",
                Some("apps/api")
            )),
            Some((
                PathBuf::from("/store/positions/repo__apps_api.jsonl.gz"),
                false
            ))
        );
        assert_eq!(
            path_of(destination_from(
                Some("1"),
                Some(Path::new("/store")),
                temp,
                "repo",
                None
            )),
            Some((PathBuf::from("/store/positions/repo.jsonl.gz"), false))
        );
    }

    /// carrick#2413: with no store the flag still walks, into a directory
    /// of this run's own under the temp dir, which goes when the service
    /// ends.
    #[test]
    fn without_a_store_the_rows_go_to_a_run_directory_that_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        for store in [None, Some(Path::new(""))] {
            let destination =
                destination_from(Some("1"), store, temp.path(), "repo", Some("apps/api"))
                    .expect("the flag walks without a store");
            assert!(destination.is_temporary());
            let dir = destination.path().parent().unwrap().to_path_buf();
            assert_eq!(dir.parent(), Some(temp.path()));
            assert_eq!(
                destination.path().file_name().unwrap(),
                "repo__apps_api.jsonl.gz"
            );
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(destination.path(), b"rows").unwrap();
            drop(destination);
            assert!(!dir.exists(), "the run directory outlived the service");
        }
    }

    fn row(kind: Kind, start: u32, end: u32, symbol: Option<&str>, flags: u8) -> Row {
        Row(
            kind,
            start,
            end,
            symbol.map(str::to_string),
            symbol.map(|_| "src/a.ts".to_string()),
            symbol.map(|_| 1),
            None,
            Some("string".to_string()),
            flags,
        )
    }

    fn file_rows(f: &str, text: Option<&str>, skip: Option<Skip>, r: Vec<Row>) -> FileRows {
        FileRows {
            f: f.to_string(),
            h: text.map(|text| content_hash(text.as_bytes())),
            skip,
            r,
        }
    }

    /// carrick#3085: an error-typed call row declared in its own file is a
    /// call that may go through an unresolved import; one declared in
    /// another file, or one that resolved, is not.
    #[test]
    fn an_error_typed_call_declared_in_its_own_file_may_be_an_unresolved_import() {
        let rows_of = |file: &str, row: Row| {
            PositionIndex::of(
                Path::new("/repo"),
                RowsFile {
                    header: Header {
                        v: FORMAT_VERSION,
                        typescript: "6.0.2".to_string(),
                        service: "api".to_string(),
                        files: 1,
                    },
                    files: vec![file_rows(file, Some("f();\n"), None, vec![row])],
                },
            )
        };
        let call = |flags: u8| row(Kind::Call, 0, 3, Some("f"), flags);
        assert!(rows_of("src/a.ts", call(FLAG_ERROR_TYPE)).may_call_through_an_unresolved_import());
        assert!(!rows_of("src/a.ts", call(0)).may_call_through_an_unresolved_import());
        assert!(
            !rows_of("src/b.ts", call(FLAG_ERROR_TYPE)).may_call_through_an_unresolved_import()
        );
        assert!(
            !rows_of(
                "src/a.ts",
                row(Kind::Ident, 0, 1, Some("f"), FLAG_ERROR_TYPE)
            )
            .may_call_through_an_unresolved_import()
        );
        assert!(
            !PositionIndex::missing(Path::new("/repo")).may_call_through_an_unresolved_import()
        );
    }

    /// Every case of carrick#2413 §3 reads its reason, and a usable row
    /// reads as itself.
    #[test]
    fn every_missing_stale_or_skipped_row_is_a_hole_with_its_reason() {
        let root = Path::new("/repo");
        let text = "const a = b;\n";
        let rows = RowsFile {
            header: Header {
                v: FORMAT_VERSION,
                typescript: "6.0.2".to_string(),
                service: "api".to_string(),
                files: 4,
            },
            files: vec![
                file_rows(
                    "src/a.ts",
                    Some(text),
                    None,
                    vec![
                        row(Kind::Ident, 10, 11, Some("b"), 0),
                        row(Kind::Ident, 6, 7, None, FLAG_ERROR_TYPE),
                        row(Kind::Ident, 0, 1, None, FLAG_CHECKER_THREW),
                        row(Kind::Member, 0, 3, None, FLAG_ANY),
                        row(Kind::Call, 0, 4, Some("f"), FLAG_ERROR_TYPE),
                    ],
                ),
                file_rows("src/stale.ts", Some("old text"), None, vec![]),
                file_rows(
                    "src/legacy.mjs",
                    Some(text),
                    Some(Skip::NotInProgram),
                    vec![],
                ),
                file_rows("src/unread.ts", Some(text), None, vec![]),
            ],
        };
        let mut index = PositionIndex::of(root, rows);
        let a = root.join("src/a.ts");
        index.bind(&a, text);
        index.bind(&root.join("src/stale.ts"), "new text");
        index.bind(&root.join("src/legacy.mjs"), text);
        let base = crate::swc_scanner::SWC_SPAN_BASE;
        let at = |lo: u32, hi: u32| (lo + base, hi + base);

        let (lo, hi) = at(10, 11);
        assert_eq!(
            index
                .declaration(&a, lo, hi, Kind::Ident, false)
                .map(Row::symbol),
            Ok(Some("b"))
        );
        // No row of the kind at the position.
        assert_eq!(
            index.lookup(&a, lo, hi, Kind::Member).err(),
            Some(HoleReason::RowsMissing)
        );
        // A file the rows do not name, and one discovery never read.
        assert_eq!(
            index
                .lookup(&root.join("src/b.ts"), lo, hi, Kind::Ident)
                .err(),
            Some(HoleReason::RowsMissing)
        );
        assert_eq!(
            index
                .lookup(&root.join("src/unread.ts"), lo, hi, Kind::Ident)
                .err(),
            Some(HoleReason::RowsMissing)
        );
        assert_eq!(
            index
                .lookup(&root.join("src/stale.ts"), lo, hi, Kind::Ident)
                .err(),
            Some(HoleReason::RowsStale)
        );
        assert_eq!(
            index
                .lookup(&root.join("src/legacy.mjs"), lo, hi, Kind::Ident)
                .err(),
            Some(HoleReason::NotInProgram)
        );
        let (lo, hi) = at(0, 1);
        assert_eq!(
            index.lookup(&a, lo, hi, Kind::Ident).err(),
            Some(HoleReason::CheckerThrew)
        );
        let (lo, hi) = at(6, 7);
        assert_eq!(
            index.declaration(&a, lo, hi, Kind::Ident, false).err(),
            Some(HoleReason::UnresolvedName)
        );
        assert_eq!(
            index.declaration(&a, lo, hi, Kind::Ident, true).err(),
            Some(HoleReason::UnresolvedImport)
        );
        let (lo, hi) = at(0, 3);
        assert_eq!(
            index.declaration(&a, lo, hi, Kind::Member, false).err(),
            Some(HoleReason::UntypedReceiver)
        );
        // The error type wins over a symbol.
        let (lo, hi) = at(0, 4);
        assert_eq!(
            index.declaration(&a, lo, hi, Kind::Call, false).err(),
            Some(HoleReason::UnresolvedName)
        );
        // No rows file at all.
        assert_eq!(
            PositionIndex::missing(root)
                .lookup(&a, lo, hi, Kind::Call)
                .err(),
            Some(HoleReason::RowsMissing)
        );
    }

    /// A span in a file with multi-byte text, CRLF line ends or a BOM joins
    /// its row: the conversion is `sidecar_position` over the bound text.
    #[test]
    fn spans_join_in_multi_byte_crlf_and_bom_files() {
        let root = Path::new("/repo");
        let base = crate::swc_scanner::SWC_SPAN_BASE;
        for text in [
            "const s = \"é€😀\";\r\nconst t = s;\r\n",
            "\u{feff}const s = \"é\";\nconst t = s;\n",
            "const s = 1;\r\nconst t = s;\r\n",
        ] {
            // The SWC span of the last `s`: bytes after any BOM.
            let body = text.strip_prefix('\u{feff}').unwrap_or(text);
            let byte = body.rfind(" s;").unwrap() as u32 + 1;
            let unit = body[..byte as usize].encode_utf16().count() as u32;
            let rows = RowsFile {
                header: Header {
                    v: FORMAT_VERSION,
                    typescript: "6.0.2".to_string(),
                    service: "api".to_string(),
                    files: 1,
                },
                files: vec![file_rows(
                    "a.ts",
                    Some(text),
                    None,
                    vec![row(Kind::Ident, unit, unit + 1, Some("s"), 0)],
                )],
            };
            let mut index = PositionIndex::of(root, rows);
            let file = root.join("a.ts");
            index.bind(&file, text);
            assert_eq!(
                index
                    .lookup(&file, byte + base, byte + 1 + base, Kind::Ident)
                    .map(Row::symbol),
                Ok(Some("s")),
                "{text:?}"
            );
        }
    }

    /// carrick#2448, #2561: the reader reads version 4 only; a file of any
    /// other version is an error, so `load_index` over a version-3 file (with
    /// its `impl` rows) left by an earlier build gives every lookup
    /// [`HoleReason::RowsMissing`] and the scan goes on.
    #[test]
    fn a_rows_file_of_another_version_is_not_read() {
        for v in [3, 5] {
            let text = format!(
                "{{\"v\":{v},\"typescript\":\"6.0.2\",\"service\":\"api\",\"files\":1}}\n\
                 {{\"f\":\"a.ts\",\"h\":\"00\",\"r\":[]}}\n"
            );
            let error = read_rows(text.as_bytes()).expect_err("another version is refused");
            assert!(error.contains("version"), "{error}");
        }
        let dir = tempfile::tempdir().unwrap();
        let destination = destination_from(Some("1"), Some(dir.path()), dir.path(), "repo", None)
            .expect("the flag walks");
        std::fs::create_dir_all(destination.path().parent().unwrap()).unwrap();
        let text = "{\"v\":3,\"typescript\":\"6.0.2\",\"service\":\"api\",\"files\":1}\n\
                    {\"f\":\"a.ts\",\"h\":\"00\",\"r\":[[\"ident\",0,1,\"a\",\"a.ts\",1,null,\"1\",0],\
                    [\"call\",0,1,\"a\",\"a.ts\",1,null,\"1\",0],\
                    [\"impl\",0,1,\"b\",\"b.ts\",1,null,null,0]]}\n";
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, text.as_bytes()).unwrap();
        std::fs::write(destination.path(), gz.finish().unwrap()).unwrap();
        let root = Path::new("/repo");
        let mut index = load_index(&destination, root, "api");
        index.bind(&root.join("a.ts"), "a");
        let base = crate::swc_scanner::SWC_SPAN_BASE;
        assert_eq!(
            index
                .lookup(&root.join("a.ts"), base, base + 1, Kind::Ident)
                .err(),
            Some(HoleReason::RowsMissing)
        );
    }

    /// carrick#2448: the `decl` rows of a member read on a union share its
    /// span and are all kept, in the order written; the `member` row keeps
    /// its own key.
    #[test]
    fn every_decl_row_at_a_span_is_kept_beside_its_member_row() {
        let root = Path::new("/repo");
        let text = "(a ? x : y).get();\n";
        let declaration = |file: &str, line: u32| {
            Row(
                Kind::Decl,
                0,
                15,
                Some("get".to_string()),
                Some(file.to_string()),
                Some(line),
                Some("svc".to_string()),
                None,
                0,
            )
        };
        let rows = RowsFile {
            header: Header {
                v: FORMAT_VERSION,
                typescript: "6.0.2".to_string(),
                service: "api".to_string(),
                files: 1,
            },
            files: vec![file_rows(
                "src/a.ts",
                Some(text),
                None,
                vec![
                    row(Kind::Member, 0, 15, Some("get"), 0),
                    declaration("src/x.ts", 2),
                    declaration("src/y.ts", 4),
                ],
            )],
        };
        let mut index = PositionIndex::of(root, rows);
        let file = root.join("src/a.ts");
        index.bind(&file, text);
        let base = crate::swc_scanner::SWC_SPAN_BASE;
        let lines: Vec<_> = index
            .union_declarations(&file, base, base + 15)
            .expect("fresh rows")
            .iter()
            .map(|r| (r.decl_file(), r.decl_line()))
            .collect();
        assert_eq!(
            lines,
            vec![(Some("src/x.ts"), Some(2)), (Some("src/y.ts"), Some(4))]
        );
        assert_eq!(
            index
                .lookup(&file, base, base + 15, Kind::Member)
                .map(Row::symbol),
            Ok(Some("get"))
        );
        assert_eq!(index.union_declarations(&file, base, base + 3), Ok(&[][..]));
    }

    #[test]
    fn a_line_with_a_key_the_format_does_not_name_is_refused() {
        let text = "{\"v\":4,\"typescript\":\"6.0.2\",\"service\":\"api\",\"files\":1}\n\
                    {\"f\":\"a.ts\",\"h\":\"00\",\"r\":[],\"extra\":1}\n";
        assert!(read_rows(text.as_bytes()).is_err());
    }

    #[test]
    fn a_row_is_a_nine_element_array() {
        let text = "{\"v\":4,\"typescript\":\"6.0.2\",\"service\":\"api\",\"files\":1}\n\
                    {\"f\":\"a.ts\",\"h\":\"00\",\"r\":[[\"call\",0,3,\"f\",\"a.ts\",1,null,\"void\",0]]}\n";
        let file = read_rows(text.as_bytes()).expect("a valid file");
        let row = &file.files[0].r[0];
        assert_eq!(row.kind(), Kind::Call);
        assert_eq!((row.start(), row.end()), (0, 3));
        assert_eq!(row.symbol(), Some("f"));
        let short = "{\"v\":4,\"typescript\":\"6.0.2\",\"service\":\"api\",\"files\":1}\n\
                     {\"f\":\"a.ts\",\"r\":[[\"call\",0,3]]}\n";
        assert!(read_rows(short.as_bytes()).is_err());
    }

    /// carrick#2504: the identifier rows whose declaration is a default-library
    /// file give the globals the code uses from each file, and the header the
    /// compiler version; a member row, and a declaration in a package, give none.
    #[test]
    fn the_index_names_the_default_library_globals_the_code_uses() {
        let text = "{\"v\":4,\"typescript\":\"6.0.2\",\"service\":\"api\",\"files\":1}\n\
                    {\"f\":\"a.ts\",\"h\":\"00\",\"r\":[\
                    [\"ident\",0,5,\"fetch\",\"lib:lib.dom.d.ts\",1,null,\"x\",0],\
                    [\"ident\",6,9,\"fetch\",\"lib:lib.dom.d.ts\",1,null,\"x\",0],\
                    [\"ident\",10,12,\"Promise\",\"lib:lib.es5.d.ts\",1,null,\"x\",0],\
                    [\"member\",13,15,\"push\",\"lib:lib.es5.d.ts\",1,null,\"x\",0],\
                    [\"ident\",16,18,\"lib\",\"node_modules/p/index.d.ts\",1,\"p\",\"x\",0]]}\n";
        let index = PositionIndex::of(Path::new("/repo"), read_rows(text.as_bytes()).unwrap());
        let (typescript, globals) = index.default_library_globals().unwrap();
        assert_eq!(typescript, "6.0.2");
        let got: Vec<(&str, Vec<&str>)> = globals
            .iter()
            .map(|(file, names)| (file.as_str(), names.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("lib.dom.d.ts", vec!["fetch"]),
                ("lib.es5.d.ts", vec!["Promise"])
            ]
        );
        assert!(
            PositionIndex::missing(Path::new("/repo"))
                .default_library_globals()
                .is_none()
        );
    }

    /// A repo of the given `(name, text)` files with rows that hash them, and
    /// the rows file written at `rows` with the record of `commit`.
    fn kept_repo(
        files: &[(&str, &str)],
        commit: Option<&str>,
    ) -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = vec![format!(
            "{{\"v\":4,\"typescript\":\"6.0.2\",\"service\":\"api\",\"files\":{}}}",
            files.len()
        )];
        let mut paths = Vec::new();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
            paths.push(dir.path().join(name));
            lines.push(format!(
                "{{\"f\":\"{name}\",\"h\":\"{}\",\"r\":[]}}",
                content_hash(text.as_bytes())
            ));
        }
        let rows = dir.path().join("kept.jsonl.gz");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, (lines.join("\n") + "\n").as_bytes()).unwrap();
        std::fs::write(&rows, gz.finish().unwrap()).unwrap();
        record_walked_at(&rows, commit);
        (dir, rows, paths)
    }

    /// carrick#3027: kept rows stand in for a walk only when nothing a walk
    /// reads has moved, and each way they can not says which.
    #[test]
    fn kept_rows_are_used_only_when_a_walk_would_write_them() {
        let (dir, rows, files) = kept_repo(&[("a.ts", "export {}\n")], Some("c1"));
        let root = dir.path();
        assert_eq!(kept_rows(&rows, "c1", root, &files), Ok(()));
        // Carried at another commit than the rows were walked from.
        assert_eq!(kept_rows(&rows, "c2", root, &files), Err(Miss::OtherCommit));
        // A file edited since.
        std::fs::write(&files[0], "export const a = 1;\n").unwrap();
        assert_eq!(
            kept_rows(&rows, "c1", root, &files),
            Err(Miss::FileChanged("a.ts".to_string()))
        );
        // A file the rows do not name.
        let (dir, rows, mut files) = kept_repo(&[("a.ts", "export {}\n")], Some("c1"));
        std::fs::write(dir.path().join("b.ts"), "export {}\n").unwrap();
        files.push(dir.path().join("b.ts"));
        assert_eq!(
            kept_rows(&rows, "c1", dir.path(), &files),
            Err(Miss::FilesDiffer)
        );
        // A file the service no longer has.
        let (dir, rows, _) = kept_repo(&[("a.ts", "export {}\n")], Some("c1"));
        assert_eq!(
            kept_rows(&rows, "c1", dir.path(), &[]),
            Err(Miss::FilesDiffer)
        );
    }

    #[test]
    fn rows_nothing_vouches_for_are_not_used() {
        // Walked from a tree that was not a clean commit: no record.
        let (dir, rows, files) = kept_repo(&[("a.ts", "export {}\n")], None);
        assert_eq!(
            kept_rows(&rows, "c1", dir.path(), &files),
            Err(Miss::NotKept)
        );
        // Another build of the scanner wrote them.
        record_walked_at(&rows, Some("c1"));
        let record = walked_at_path(&rows);
        let text = std::fs::read_to_string(&record).unwrap();
        std::fs::write(
            &record,
            text.replacen(env!("CARGO_PKG_VERSION"), "0.0.0", 1),
        )
        .unwrap();
        assert_eq!(
            kept_rows(&rows, "c1", dir.path(), &files),
            Err(Miss::OtherBuild)
        );
        // No rows file at all.
        std::fs::remove_file(&rows).unwrap();
        record_walked_at(&rows, Some("c1"));
        assert!(matches!(
            kept_rows(&rows, "c1", dir.path(), &files),
            Err(Miss::Unreadable(_))
        ));
    }

    #[test]
    fn a_file_the_walk_could_not_read_or_hash_is_a_miss() {
        let (dir, rows, files) = kept_repo(&[("a.ts", "export {}\n")], Some("c1"));
        let mut kept = read_rows_file(&rows).unwrap();
        kept.files[0].h = None;
        assert_eq!(
            kept_rows_match(&kept, dir.path(), &files),
            Err(Miss::FileChanged("a.ts".to_string()))
        );
    }

    #[test]
    fn rows_are_kept_in_the_named_directory_else_beside_the_store() {
        let (kept, store) = (Path::new("/kept"), Path::new("/store"));
        let path = |kept, store| kept_path_from(kept, store, "repo", Some("api"));
        assert_eq!(
            path(Some(kept), Some(store)),
            Some(PathBuf::from("/kept/repo__api.jsonl.gz"))
        );
        assert_eq!(
            path(None, Some(store)),
            Some(PathBuf::from("/store/positions/repo__api.jsonl.gz"))
        );
        // The Action and the cloud runner keep nothing.
        assert_eq!(path(None, None), None);
        assert_eq!(path(Some(Path::new("")), Some(Path::new(""))), None);
    }
}
