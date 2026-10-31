//! What the tree being scanned looks like to git, and what a laptop run says
//! about it before it uploads.
//!
//! A CI scan runs on a checkout that git made: HEAD is the commit the workflow
//! was dispatched for and nothing is modified. A laptop scan runs on whatever
//! the developer has open, so the index it produces can describe a branch, or
//! code that exists in no commit at all. The upload carries `commit_hash`
//! either way, and without these two facts beside it that field is a claim the
//! index cannot support.
//!
//! The policy is warn, never refuse (David's ruling, 2026-09-11): both
//! conditions are reported separately, the scan proceeds, and `dirty` rides
//! the blob so every later reader can say so. Wire contract:
//! carrick-cloud `docs/internal/reference/laptop-scan-seam.md` §8.4.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// What git says about the tree this run is scanning.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitState {
    /// HEAD's commit, or empty when this is not a git repository.
    pub commit: String,
    /// The branch HEAD is on, or `None` on a detached HEAD.
    pub branch: Option<String>,
    /// The working tree carries changes HEAD does not describe.
    pub dirty: bool,
    /// HEAD is `origin/main`'s commit. `None` when `origin/main` does not
    /// resolve in this clone, which is not the same as "it differs".
    pub at_origin_main: Option<bool>,
}

/// Ask git about `repo_path`.
///
/// Every call clears the inherited git environment for the same reason
/// [`crate::cloud_storage::get_current_commit_hash`] does: a pre-commit hook
/// or a harness subprocess can carry a `GIT_DIR` for a different repository,
/// and this would then describe that one.
pub fn inspect(repo_path: &str) -> GitState {
    // Untrimmed: a porcelain status entry starts with a space when only the
    // working tree changed, and trimming the answer would shift the first one.
    let run_raw = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let run = |args: &[&str]| run_raw(args).map(|out| out.trim().to_string());

    let commit = run(&["rev-parse", "HEAD"]).unwrap_or_default();
    if commit.is_empty() {
        return GitState::default();
    }
    let branch = run(&["rev-parse", "--abbrev-ref", "HEAD"]).filter(|name| name != "HEAD");
    // One entry per path git considers changed, staged or untracked. Every
    // untracked file is listed on its own (`--untracked-files=all`), because a
    // wholly new `.claude/` otherwise collapses to one entry that cannot be
    // told apart from any other directory. The paths are relative to the
    // repository's top level whatever directory this runs in, so the scan
    // root's own position is asked for beside them.
    let prefix = run(&["rev-parse", "--show-prefix"]).unwrap_or_default();
    let dirty = run_raw(&[
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--no-renames",
    ])
    .is_some_and(|out| carries_changes_beyond_the_scaffold(&out, &prefix));
    // `None`, not `false`, when the ref is absent: a clone with no
    // `origin/main` cannot say whether HEAD is at it, and saying "not at
    // origin/main" there would warn every user of a repo whose default branch
    // is called something else.
    let at_origin_main = run(&["rev-parse", "origin/main"]).map(|main| main == commit);

    GitState {
        commit,
        branch,
        dirty,
        at_origin_main,
    }
}

/// Files the Carrick scaffold writes into a repository that no scan reads.
///
/// The ruled first run (carrick#987) has an agent write these, then scan,
/// before any of them is committed, so a tree whose only changes are these
/// would otherwise read dirty on every run until the scaffold PR merges: the
/// warning on every run, and a blob every other clone refuses (carrick#1024,
/// carrick#1117). A change to any of them, new or edited, leaves what the
/// index describes exactly what the commit holds.
///
/// Mirrors `npm/carrick/src/init/remove.ts` (`SCAFFOLD_FILES`,
/// `SETTINGS_FILES` and the files `SECTION_CARRIERS` merges into),
/// `npm/carrick/src/init/codex.ts` (`CODEX_HOOKS_FILE`) and
/// `npm/carrick/src/init/task-skills.ts` (`SKILL_ROOTS` x `TASK_SKILLS`),
/// which mirror the scaffold tool in carrick-cloud
/// (`lambdas/mcp-server/src/tools/scaffold.ts`). A file added there is added
/// here too, or a first run is dirty again.
const SCAFFOLD_FILES: &[&str] = &[
    ".github/workflows/carrick.yml",
    ".claude/skills/carrick/SKILL.md",
    ".claude/skills/carrick-impact/SKILL.md",
    ".claude/skills/carrick-reuse/SKILL.md",
    ".claude/skills/carrick-drift/SKILL.md",
    ".claude/skills/carrick-census/SKILL.md",
    ".agents/skills/carrick-impact/SKILL.md",
    ".agents/skills/carrick-reuse/SKILL.md",
    ".agents/skills/carrick-drift/SKILL.md",
    ".agents/skills/carrick-census/SKILL.md",
    ".claude/session-start.sh",
    ".claude/turn-reminder.sh",
    ".claude/search-gate.sh",
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".codex/hooks.json",
    "AGENTS.md",
    "CLAUDE.md",
    ".gitignore",
];

/// The scaffold's config, which a scan DOES read. It is part of the scaffold
/// only while the commit does not hold it at all: a `carrick.json` the tree
/// adds is the first run's, and an edit to a committed one is a change to what
/// the index describes, so it still counts.
const SCAFFOLD_CONFIG: &str = "carrick.json";

/// Whether a `git status --porcelain=v1 -z` answer names any change that is
/// not the Carrick scaffold's.
///
/// `prefix` is the scan root's path from the repository's top level (`sub/`,
/// or empty at the top), because the scaffold lands where the agent was told
/// to write it: the root of the repo it scanned. Paths are matched at the top
/// level and at that prefix, and `.carrick/` anywhere, which is the ignored
/// directory `carrick init` and `carrick index` write.
///
/// And only a change to a file a scan reads (carrick#2826, David's ruling
/// 2026-10-25): a source file the walk collects, a repo input, or a
/// TypeScript or JavaScript config. A tree whose only changes are an editor's
/// settings, a README or a workflow holds what the commit holds for every
/// file the index was read from, so its blob is clean and a later
/// `--changed-only` run can carry it. The same filter `carrick status` counts
/// drift by (carrick#1007 item 5), plus the repo inputs and configs a scan
/// also reads.
fn carries_changes_beyond_the_scaffold(porcelain: &str, prefix: &str) -> bool {
    porcelain
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .any(|entry| {
            let (status, path) = entry.split_at(3);
            !is_scaffold_change(status.trim_end(), path, prefix) && a_scan_reads(path)
        })
}

/// Whether a scan reads the file at `path`, top-relative: a source file the
/// walk collects, a repo input every service reads
/// ([`crate::change_scope::is_repo_input`]), or a `tsconfig`/`jsconfig`.
fn a_scan_reads(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    crate::file_finder::is_scanned_source(Path::new(path), Path::new(""))
        || crate::change_scope::is_repo_input(path)
        || ((name.starts_with("tsconfig") || name.starts_with("jsconfig"))
            && (name.ends_with(".json") || name.ends_with(".jsonc")))
}

fn is_scaffold_change(status: &str, path: &str, prefix: &str) -> bool {
    if path.starts_with(".carrick/") || path.contains("/.carrick/") {
        return true;
    }
    let at_root = |name: &str| path == name || path.strip_prefix(prefix) == Some(name);
    if SCAFFOLD_FILES.iter().any(|name| at_root(name)) {
        return true;
    }
    // `??` untracked, or `A` staged as a new file (with or without a later
    // edit): either way the commit has no copy of it.
    at_root(SCAFFOLD_CONFIG) && (status == "??" || status.starts_with('A'))
}

/// The paths under `repo_path` whose content on disk is the content `commit`
/// holds for them, relative to `repo_path`.
///
/// The one question the incremental analysis cache asks, from both ends
/// (carrick#1079). The reader replays a previous scan's answer for a file only
/// when the file is in this set for that scan's commit; the writer keeps an
/// answer only when the file is in this set for HEAD. An answer that survives
/// both therefore describes bytes a commit names, which is what lets a dirty
/// tree keep its cache for every file it did not touch instead of dropping all
/// of it.
///
/// Built from what git tracks, minus what differs from `commit` in the working
/// tree (`git diff <commit> --` compares the commit to the files on disk, so
/// staged and unstaged edits both count). A file git does not track is never
/// in the set: untracked and ignored files alike have no content any commit
/// can vouch for, and the scanner's own walk does not read `.gitignore`.
///
/// Both git calls run from `repo_path` and print paths relative to it
/// (`ls-files` does by default, `diff` with `--relative`), which is the form
/// the cache is keyed by even when `repo_path` is a directory inside a larger
/// repository. `-z` keeps a non-ASCII path byte-exact rather than quoted.
///
/// `Err` carries git's reason and means "git could not answer" (not a
/// repository, a commit this clone does not hold, a shallow history), which is
/// deliberately distinct from an empty set.
pub fn unchanged_since(repo_path: &str, commit: &str) -> Result<HashSet<String>, String> {
    // A value that is not a hex object name never reaches git's argument list,
    // where a leading `-` would read as an option.
    if commit.is_empty() || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{commit:?} is not a commit id"));
    }
    // On a partial clone, `git diff` compares an edited tracked file's bytes
    // with the commit's copy, and that copy may be absent: git then fetches it
    // from the promisor remote, and an unreachable remote fails the whole
    // diff. With the stat-refresh off the diff compares object ids only, so an
    // edited file reads as changed and nothing is fetched (carrick#2212). A
    // full clone keeps git's default, which never fetches.
    let mut diff_args = vec![];
    if is_partial_clone(repo_path) {
        diff_args.extend(["-c", "diff.autoRefreshIndex=false"]);
    }
    diff_args.extend([
        "diff",
        "--name-only",
        "--no-renames",
        "--relative",
        "-z",
        commit,
        "--",
    ]);
    let changed: HashSet<String> = nul_paths(run_git(repo_path, &diff_args)?)
        .into_iter()
        .collect();
    Ok(nul_paths(run_git(repo_path, &["ls-files", "-z"])?)
        .into_iter()
        .filter(|path| !changed.contains(path))
        .collect())
}

/// The ref an indexed commit fetched by [`fetch_indexed_commit`] is kept under.
/// Carrick owns it, so `git gc` keeps the commit and no branch of the user's
/// moves.
pub const INDEXED_REF: &str = "refs/carrick/indexed";

/// The longest a fetch of the indexed commit may run.
const FETCH_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether `commit` is a commit object this clone holds.
pub fn has_commit(repo_path: &str, commit: &str) -> bool {
    commit.bytes().all(|b| b.is_ascii_hexdigit())
        && !commit.is_empty()
        && run_git(
            repo_path,
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        )
        .is_ok()
}

/// Fetch `commit` from `origin` by its hash and keep it under [`INDEXED_REF`]
/// (carrick#3000), so a clone that has not pulled the commit an index was
/// written at (a PR squash-merged on GitHub) can still compare against it.
///
/// One `git fetch` of that one commit, never a prompt, and abandoned after
/// [`FETCH_LIMIT`]. A shallow clone is fetched at depth 1, so no history
/// comes with it; a full clone is not made shallow by it, and receives only
/// what the commit adds to history it already holds. `Err` carries the
/// reason, on one line: offline, no access, no `origin`, or a server that
/// does not serve an object by its hash.
pub fn fetch_indexed_commit(repo_path: &str, commit: &str) -> Result<(), String> {
    if commit.is_empty() || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{commit:?} is not a commit id"));
    }
    let shallow = run_git(repo_path, &["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|out| out.trim() == "true");
    let refspec = format!("+{commit}:{INDEXED_REF}");
    let mut command = Command::new("git");
    command
        .args(["fetch", "--no-tags", "--quiet", "--no-recurse-submodules"])
        .args(shallow.then_some("--depth=1"))
        .args(["origin", &refspec])
        .current_dir(repo_path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut why = String::new();
                if let Some(mut err) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut err, &mut why);
                }
                let why = why.split_whitespace().collect::<Vec<_>>().join(" ");
                return Err(if why.is_empty() {
                    "git fetch failed".to_string()
                } else {
                    why
                });
            }
            Ok(None) if started.elapsed() >= FETCH_LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git fetch took longer than {}s",
                    FETCH_LIMIT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// One run of lines a diff replaces: `old_count` lines from `old_start` in the
/// commit's copy became `new_count` lines from `new_start` in the working
/// tree. Either count can be zero (a pure insertion or deletion); a zero
/// `old_count` places the insertion AFTER line `old_start`, as git states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
}

/// Parse one `@@ -a[,b] +c[,d] @@` header. A missing count is one line.
pub fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let body = line.strip_prefix("@@ -")?;
    let (ranges, _) = body.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
    })
}

/// The hunks that turn `path` as `commit` holds it into the copy `to` holds
/// (another commit, or the working tree when `None`), in file order, with no
/// context lines (carrick#2894).
///
/// Run from `repo_path`, with `path` relative to it, read literally (a
/// `[id].ts` route is not a glob). Guarded as [`unchanged_since`] is: a value
/// that is not a hex object name never reaches git, a partial clone's
/// stat-refresh is off, and no blob is fetched. `Err` is "git could not
/// answer", which is not the same as no hunks.
pub fn changed_hunks(
    repo_path: &str,
    commit: &str,
    path: &str,
    to: Option<&str>,
) -> Result<Vec<Hunk>, String> {
    for id in std::iter::once(commit).chain(to) {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{id:?} is not a commit id"));
        }
    }
    let mut args = vec![];
    if is_partial_clone(repo_path) {
        args.extend(["-c", "diff.autoRefreshIndex=false"]);
    }
    args.extend([
        "diff",
        "-U0",
        "--no-color",
        "--no-ext-diff",
        "--no-renames",
        "--relative",
        commit,
    ]);
    args.extend(to);
    args.extend(["--", path]);
    let output = Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_hunk_header).collect())
}

/// Where line `old_line` of the commit's copy sits in the working tree's, or
/// `None` when a hunk replaced it. Lines outside every hunk move by the net
/// change of the hunks above them.
pub fn map_line(hunks: &[Hunk], old_line: u32) -> Option<u32> {
    let mut shift: i64 = 0;
    for hunk in hunks {
        if hunk.old_count == 0 {
            // Inserted after `old_start`: it moves only the lines below it.
            if hunk.old_start < old_line {
                shift += i64::from(hunk.new_count);
            } else {
                break;
            }
        } else if old_line < hunk.old_start {
            break;
        } else if old_line < hunk.old_start + hunk.old_count {
            return None;
        } else {
            shift += i64::from(hunk.new_count) - i64::from(hunk.old_count);
        }
    }
    u32::try_from(i64::from(old_line) + shift).ok()
}

/// Whether the checkout at `repo_path` is a partial clone: git's own marker,
/// `extensions.partialClone` or a remote with `promisor = true`.
fn is_partial_clone(repo_path: &str) -> bool {
    let extension = run_git(repo_path, &["config", "--get", "extensions.partialClone"])
        .is_ok_and(|out| !out.trim().is_empty());
    extension
        || run_git(
            repo_path,
            &["config", "--get-regexp", r"^remote\..*\.promisor$"],
        )
        .is_ok_and(|out| out.lines().any(|line| line.ends_with(" true")))
}

/// The parents of the commit checked out at `repo_path`, first parent first.
///
/// Read from the raw commit object, so a shallow clone (whose history git
/// otherwise treats as ending at HEAD) still names them. `Err` means git could
/// not answer.
pub fn head_parents(repo_path: &str) -> Result<Vec<String>, String> {
    Ok(run_git(repo_path, &["cat-file", "-p", "HEAD"])?
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.strip_prefix("parent "))
        .map(str::to_string)
        .collect())
}

/// The commit checked out at `repo_path` (carrick#2229).
pub fn head_commit(repo_path: &str) -> Result<String, String> {
    Ok(run_git(repo_path, &["rev-parse", "HEAD"])?
        .trim()
        .to_string())
}

/// Whether the trees of commits `a` and `b` differ under `repo_path`.
///
/// `Err` when either is not a commit id this clone holds, or git could not
/// answer: a commit that is not here cannot be compared, which is not the
/// same as one that differs.
///
/// Answered from the two trees alone, so a partial clone that holds them but
/// not every file's contents answers offline (carrick#2365). `diff-tree` is
/// plumbing: it compares entries by object id and reads no diff driver,
/// textconv or external diff, which porcelain `git diff --quiet` may run and
/// which make it fetch missing contents.
pub fn differs_under(repo_path: &str, a: &str, b: &str) -> Result<bool, String> {
    for commit in [a, b] {
        // A value that is not a hex object name never reaches git's argument
        // list, where a leading `-` would read as an option.
        if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("{commit:?} is not a commit id"));
        }
        run_git(
            repo_path,
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        )?;
    }
    let output = Command::new("git")
        .args([
            "diff-tree",
            "--quiet",
            "-r",
            "--no-renames",
            a,
            b,
            "--",
            ".",
        ])
        .current_dir(repo_path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|e| e.to_string())?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(format!(
            "git diff-tree exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// One path `git diff --name-status` lists, relative to the repository's top
/// level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPath {
    /// git's status letter: `A`dded, `D`eleted, `M`odified, `T`ype changed.
    pub status: char,
    pub path: String,
}

impl ChangedPath {
    /// Added or deleted: a change that can alter what an unchanged import
    /// resolves to.
    pub fn adds_or_deletes(&self) -> bool {
        matches!(self.status, 'A' | 'D')
    }
}

/// Every path that differs between commit `from` and `to` (another commit,
/// or the working tree when `None`), relative to the repository's top level
/// whatever directory `repo_path` is (carrick#2227).
///
/// No `--relative` and no pathspec: a scan rooted in a subdirectory still
/// sees a change outside it, which is where a service's imports can reach.
/// Against the working tree, a file git does not track yet is listed as
/// added. `Err` means git could not compare: a commit this clone does not
/// hold, or no answer.
pub fn changed_paths(
    repo_path: &str,
    from: &str,
    to: Option<&str>,
) -> Result<Vec<ChangedPath>, String> {
    for commit in std::iter::once(from).chain(to) {
        // A value that is not a hex object name never reaches git's argument
        // list, where a leading `-` would read as an option.
        if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("{commit:?} is not a commit id"));
        }
        run_git(
            repo_path,
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        )?;
    }
    // The partial-clone guard `unchanged_since` explains (carrick#2212).
    let mut args = vec![];
    if to.is_none() && is_partial_clone(repo_path) {
        args.extend(["-c", "diff.autoRefreshIndex=false"]);
    }
    args.extend(["diff", "--name-status", "--no-renames", "-z", from]);
    args.extend(to);
    args.push("--");
    let fields = nul_paths(run_git(repo_path, &args)?);
    let mut changed: Vec<ChangedPath> = fields
        .as_chunks::<2>()
        .0
        .iter()
        .filter_map(|pair| {
            Some(ChangedPath {
                status: pair[0].chars().next()?,
                path: pair[1].clone(),
            })
        })
        .collect();
    if to.is_none() {
        changed.extend(
            nul_paths(run_git(
                repo_path,
                &[
                    "ls-files",
                    "--others",
                    "--exclude-standard",
                    "--full-name",
                    "-z",
                ],
            )?)
            .into_iter()
            .map(|path| ChangedPath { status: 'A', path }),
        );
    }
    Ok(changed)
}

/// Every path under `repo_path` that git does not track, ignored or not,
/// relative to the repository's top level: a file, or a folder written with a
/// trailing `/` when nothing under it is tracked. Empty folders are left out.
/// `Err` means git could not answer.
pub fn untracked_paths(repo_path: &str) -> Result<Vec<String>, String> {
    Ok(nul_paths(run_git(
        repo_path,
        &[
            "ls-files",
            "--others",
            "--directory",
            "--no-empty-directory",
            "--full-name",
            "-z",
        ],
    )?))
}

/// The top-level-relative path of the scan root's `carrick.json` when the
/// tree holds one that no commit does: untracked, and absent from HEAD. The
/// runner writes the project config there before it scans, so it is the
/// scaffold's own file (`SCAFFOLD_CONFIG`), not an edit the commit under scan
/// made. `None` for a tracked or ignored file, or when git cannot say.
pub fn untracked_root_config(repo_path: &str) -> Option<String> {
    let listed = nul_paths(
        run_git(
            repo_path,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "--full-name",
                "-z",
                "--",
                SCAFFOLD_CONFIG,
            ],
        )
        .ok()?,
    );
    let path = listed.into_iter().next()?;
    // Untracked says the index lacks it; HEAD must lack it too.
    run_git(repo_path, &["cat-file", "-e", "HEAD:./carrick.json"])
        .is_err()
        .then_some(path)
}

/// The text of `path` (relative to the repository's top level) as `commit`
/// holds it, or `None` when this clone does not hold the commit or the file.
/// A blobless clone is not asked to fetch: `GIT_NO_LAZY_FETCH` makes a missing
/// blob fail at once rather than reach for a remote (carrick#2316).
pub fn file_at_commit(repo_path: &str, commit: &str, path: &str) -> Option<String> {
    if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let output = Command::new("git")
        .args(["show", &format!("{commit}:{path}")])
        .current_dir(repo_path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

/// One file that differs between two trees, with the path it was renamed
/// from when git pairs it with one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// git's status letter: `A`dded, `D`eleted, `M`odified, `R`enamed,
    /// `T`ype changed.
    pub status: char,
    /// The path in the newer tree, or the deleted file's path, relative to
    /// the repository's top level.
    pub path: String,
    /// The path in `from`, for a renamed file.
    pub previous: Option<String>,
}

/// The best common ancestor of commits `a` and `b`: where the history of one
/// left the other's. It is `a` itself when `b` descends from it. `Err` when
/// either is not a commit this clone holds, or its history stops short of a
/// common ancestor (a shallow clone).
pub fn merge_base(repo_path: &str, a: &str, b: &str) -> Result<String, String> {
    for commit in [a, b] {
        // As in `changed_paths`: nothing but a hex object name reaches git.
        if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("{commit:?} is not a commit id"));
        }
    }
    let base = run_git(repo_path, &["merge-base", a, b])?
        .trim()
        .to_string();
    if base.is_empty() {
        return Err(format!("{a} and {b} share no history in this clone"));
    }
    Ok(base)
}

/// Whether `ancestor` is `descendant` or a commit in its history. `false` when
/// either is not a full commit id this clone holds: an answer git could not
/// give is never a yes.
pub fn is_ancestor(repo_path: &str, ancestor: &str, descendant: &str) -> bool {
    merge_base(repo_path, ancestor, descendant).is_ok_and(|base| base == ancestor)
}

/// The repo's main branch, as a ref this clone can resolve: the remote's
/// default (`origin/HEAD`) where the clone recorded one, and otherwise the
/// first of `origin/main`, `origin/master`, `main`, `master` that exists.
/// `None` when none does.
pub fn main_branch(repo_path: &str) -> Option<String> {
    if let Ok(text) = run_git(
        repo_path,
        &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
    ) {
        let name = text.trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|name| {
            run_git(
                repo_path,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{name}^{{commit}}"),
                ],
            )
            .is_ok()
        })
        .map(str::to_string)
}

/// Where the checked-out work left the repo's main branch: the merge base of
/// `HEAD` and [`main_branch`], with the branch's name. Everything committed on
/// the branch since, plus whatever is uncommitted, is the work in hand.
pub fn main_base(repo_path: &str) -> Result<(String, String), String> {
    let branch = main_branch(repo_path).ok_or(
        "no main branch to compare against (looked for origin/HEAD, origin/main, \
         origin/master, main and master); run `git fetch origin main`",
    )?;
    let tip = run_git(
        repo_path,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    )?
    .trim()
    .to_string();
    let head = head_commit(repo_path)?;
    match merge_base(repo_path, &head, &tip) {
        Ok(base) => Ok((branch, base)),
        Err(_) => Err(no_base_message(repo_path, &branch)),
    }
}

/// One line for a clone with no merge base against `branch`: the ref tried
/// and the command that fixes it. A shallow clone is told to unshallow; any
/// other is told to fetch the branch (unrelated history needs the real one).
fn no_base_message(repo_path: &str, branch: &str) -> String {
    let shallow = run_git(repo_path, &["rev-parse", "--is-shallow-repository"])
        .map(|text| text.trim() == "true")
        .unwrap_or(false);
    if shallow {
        format!(
            "no merge base with {branch}: this clone is shallow, so its history stops short of \
             it; run `git fetch --unshallow`"
        )
    } else {
        let remote_branch = branch.strip_prefix("origin/").unwrap_or(branch);
        format!(
            "no merge base with {branch}: this clone shares no history with it; run \
             `git fetch origin {remote_branch}`"
        )
    }
}

/// Every file that differs between commits `from` and `to`, as
/// [`changed_paths`] lists them but with a renamed file paired to its old
/// path (carrick#2285), for a reader that compares a file's two copies. Kept
/// apart because the scope rule reads an addition and a deletion where this
/// names a rename. Two commits and never the working tree: what a build step
/// wrote there is no part of either.
///
/// Pairing renames reads file contents. A partial clone (the cloud runner's)
/// is read from the copies it holds and never fetches one (carrick#2285): the
/// runner fetches the changed files on both sides before the scan, which
/// holds no credential to fetch more. A copy the clone lacks is an `Err`, and
/// so is a git too old to read a partial clone without fetching. The clone
/// this needs is stated in `docs/reference/runner.md`, "Clone shape".
pub fn changed_files(repo_path: &str, from: &str, to: &str) -> Result<Vec<FileChange>, String> {
    // Before any read: a git that ignores `NO_LAZY_FETCH` would fetch.
    if is_partial_clone(repo_path) && run_git(repo_path, &["--no-lazy-fetch", "version"]).is_err() {
        return Err("a partial clone needs git 2.44 or later to read without fetching".to_string());
    }
    for commit in [from, to] {
        // As in `changed_paths`: nothing but a hex object name reaches git.
        if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("{commit:?} is not a commit id"));
        }
        run_git_env(
            repo_path,
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
            &NO_LAZY_FETCH,
        )?;
    }
    let args = ["diff", "--name-status", "-M", "-z", from, to, "--"];
    let mut fields = nul_paths(run_git_env(repo_path, &args, &NO_LAZY_FETCH)?).into_iter();
    let mut changed = Vec::new();
    while let Some(status) = fields.next() {
        let Some(letter) = status.chars().next() else {
            continue;
        };
        // A rename or a copy states two paths, the old one first.
        let previous = if matches!(letter, 'R' | 'C') {
            fields.next()
        } else {
            None
        };
        let Some(path) = fields.next() else { break };
        changed.push(FileChange {
            status: letter,
            path,
            // A copy leaves its source in place, so the new file is an
            // addition with nothing behind it.
            previous: previous.filter(|_| letter == 'R'),
        });
    }
    Ok(changed)
}

/// The text of `path` (relative to the repository's top level) as `commit`
/// holds it. Bytes that are not UTF-8 are replaced, so one odd file reads as
/// text that does not parse and not as git failing. `Err` means git could not
/// read it, which on a partial clone includes a copy it does not hold: as in
/// [`changed_files`], nothing is fetched.
pub fn file_at(repo_path: &str, commit: &str, path: &str) -> Result<String, String> {
    if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{commit:?} is not a commit id"));
    }
    let output = Command::new("git")
        .args(["show", &format!("{commit}:{path}")])
        .current_dir(repo_path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .envs(NO_LAZY_FETCH)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The repository's top level, as git names it, for the checkout at
/// `repo_path`.
pub fn toplevel(repo_path: &str) -> Result<std::path::PathBuf, String> {
    Ok(std::path::PathBuf::from(
        run_git(repo_path, &["rev-parse", "--show-toplevel"])?.trim(),
    ))
}

/// The tracked paths under `repo_path` that match any of `pathspecs` (git
/// pathspec globs such as `*.graphql`), relative to `repo_path`.
///
/// "Tracked" is the index's answer, so a file that is committed or staged
/// counts and an untracked or ignored one does not: a printed schema a
/// developer generated locally and never committed is not part of what the
/// repository states (carrick#1134). `Err` means git could not answer, as for
/// [`unchanged_since`].
pub fn tracked_paths(repo_path: &Path, pathspecs: &[&str]) -> Result<Vec<String>, String> {
    let repo_path = repo_path.to_string_lossy();
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend_from_slice(pathspecs);
    Ok(nul_paths(run_git(&repo_path, &args)?))
}

/// What git ignores in the checkout at `repo_path` and does not track
/// (carrick#1902), relative to it: a file, or a folder written with a
/// trailing `/` when nothing under it is tracked or kept.
///
/// The repository's `.gitignore` files and this machine's own excludes
/// (`.git/info/exclude`, `core.excludesFile`) both count. A tracked file that
/// matches an ignore pattern is not listed. `Err` means git could not answer,
/// as for [`unchanged_since`].
pub fn ignored_paths(repo_path: &Path) -> Result<Vec<String>, String> {
    Ok(nul_paths(run_git(
        &repo_path.to_string_lossy(),
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
    )?))
}

/// Set on a read that must not fetch: git answers from the objects the clone
/// holds, and an absent one is an error, never a fetch from a partial clone's
/// remote. Nothing is fetched lazily from a full clone, so there it changes
/// nothing. Honoured by git 2.44 and later; [`changed_files`] refuses an
/// older git on a partial clone before reading.
const NO_LAZY_FETCH: [(&str, &str); 1] = [("GIT_NO_LAZY_FETCH", "1")];

/// Run git in `repo_path` with the inherited git environment cleared (see
/// [`inspect`]), returning stdout, or stderr as the error.
fn run_git(repo_path: &str, args: &[&str]) -> Result<String, String> {
    run_git_env(repo_path, args, &[])
}

/// [`run_git`] with `env` set on the git process.
fn run_git_env(repo_path: &str, args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .envs(env.iter().copied())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

/// Split `-z` output into paths.
fn nul_paths(text: String) -> Vec<String> {
    text.split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// A clone's `origin` remote, and the repository it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The remote as git holds it, with any credentials taken out: a remote
    /// can carry a token in its userinfo half, and this string is printed to
    /// the reader and stored in the local index.
    pub url: String,
    /// `owner/repo`, or `None` when the remote names no such path. The reason
    /// travels with the answer so a surface can say which of the two it is
    /// (carrick#991, carrick#1056).
    pub name: Option<String>,
}

/// This clone's `origin` remote, or `None` when it has none.
pub fn origin(repo: &Path) -> Option<Origin> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", "origin"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8(output.stdout).ok()?;
    let url = url.trim();
    Some(Origin {
        name: parse_remote(url),
        url: without_credentials(url),
    })
}

/// `owner/repo` from this clone's `origin` remote, or `None` when it has none
/// or the remote names no `owner/repo` path.
///
/// The only identity a laptop can offer the cloud: CI derives it from the
/// signed OIDC claims instead, and never calls this. The hosted index reader
/// resolves the same name the same way, which is what makes a repo the CLI
/// sees the repo the cloud answers about, so there is one copy of it.
pub fn remote_name(repo: &Path) -> Option<String> {
    origin(repo)?.name
}

/// `owner/repo` from a remote URL in any of the forms git writes, whatever
/// host it names.
///
/// The host is not a gate. A machine with two GitHub accounts writes its
/// remotes through a per-account ssh alias — `git@github.com-personal:owner/repo`,
/// a name only that user's `~/.ssh/config` can resolve — and that is an
/// ordinary GitHub repository. Refusing it left a repo the cloud had just
/// connected reading as "not connected to a Carrick project" on the machine
/// that connected it (carrick#1056). Whether the cloud knows a name is the
/// cloud's answer: `resolve-repos` returns nothing for a name outside the
/// workspace and `start-scan` refuses a repo this holder may not write, so
/// this reads the name and decides nothing else.
pub fn parse_remote(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let path = if has_scheme(remote) {
        // Parsed rather than split so a port, a userinfo half or an escape
        // cannot be read as part of the path.
        let url = reqwest::Url::parse(remote).ok()?;
        // A URL naming no host names no repository host: `file:///tmp/api`.
        if url.host_str().is_none_or(str::is_empty) {
            return None;
        }
        url.path().to_string()
    } else {
        // The scp spelling `[user@]host:owner/repo`, which is not a URL. The
        // user half is optional and never read — git writes an alias with no
        // user as `github.com-personal:owner/repo`.
        let after_user = remote.split_once('@').map_or(remote, |(_, rest)| rest);
        let (host, path) = after_user.split_once(':')?;
        if host.is_empty() || host.contains('/') {
            return None;
        }
        path.to_string()
    };
    // The last two segments, so a host that nests groups names its repository
    // rather than nothing. A `.git` suffix is git's, not part of the name.
    let mut segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let repo = segments.pop()?;
    let owner = segments.pop()?;
    let name = format!("{owner}/{}", repo.strip_suffix(".git").unwrap_or(repo));
    valid_repo_name(&name).then_some(name)
}

/// Whether a remote is written in a URL form (`scheme://…`) rather than the
/// scp one. Told apart by the `//`, not by whether a URL parser accepts it: a
/// host alias with no user (`github.com-personal:owner/repo`) parses as a URL
/// whose scheme is the host, and would otherwise be read as neither form.
fn has_scheme(remote: &str) -> bool {
    let Some((scheme, _)) = remote.split_once("://") else {
        return false;
    };
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

/// A remote with its userinfo removed, for printing.
///
/// A URL-form remote can carry a token (`https://x-access-token:TOKEN@host/…`),
/// and the one remote this module prints is the one it could not read a name
/// from — which includes a credential-carrying URL with no repository path.
fn without_credentials(remote: &str) -> String {
    if !has_scheme(remote) {
        return remote.to_string();
    }
    let Ok(mut url) = reqwest::Url::parse(remote) else {
        return remote.to_string();
    };
    if url.username().is_empty() && url.password().is_none() {
        return remote.to_string();
    }
    if url.set_username("").is_err() || url.set_password(None).is_err() {
        // A URL that cannot hold a username cannot be stripped of one, and
        // printing it as it stands would print the credential.
        return "a remote carrying credentials".to_string();
    }
    url.to_string()
}

/// Whether a string is a `owner/repo` name and nothing else — no path escape,
/// no empty half.
pub fn valid_repo_name(name: &str) -> bool {
    let parts: Vec<_> = name.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}

/// The repo-relative short form the warning names HEAD by.
fn short(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// The line every laptop run ends its warnings on. CI is where a routine scan
/// belongs: it runs on a clean checkout of the default branch, and its rows
/// supersede a laptop's.
pub const LEAVE_ROUTINE_SCANS_TO_CI: &str = "Leave routine scans to CI: a CI scan runs on a clean checkout of the default branch, \
     and its index replaces whatever a laptop scan wrote.";

/// What this run warns about before it uploads, in order, or empty when there
/// is nothing to say.
///
/// Pure so the wording is pinned by a test without a git repository. Both
/// conditions are separate and both are reported: a dirty tree on a feature
/// branch is two problems, not one.
pub fn warnings(state: &GitState) -> Vec<String> {
    let mut lines = Vec::new();
    if state.at_origin_main == Some(false) {
        lines.push(format!(
            "This tree is at {} on {}, not origin/main. The index will describe this branch \
             until the next CI scan.",
            short(&state.commit),
            state.branch.as_deref().unwrap_or("a detached HEAD"),
        ));
    }
    if state.dirty {
        // The cost clause is the part a laptop run cannot see for itself: an
        // uncommitted file keeps no cache entry, so the next scan, CI's
        // included, asks the model about it again (carrick#993 row 17). Every
        // file the change did not touch keeps its entry (carrick#1079).
        lines.push(
            "This tree has uncommitted changes. They will be indexed, and marked as such. \
             Cached analysis will not be kept for the changed files, and the next CI scan \
             re-analyses them."
                .to_string(),
        );
    }
    if !lines.is_empty() {
        lines.push(LEAVE_ROUTINE_SCANS_TO_CI.to_string());
    }
    lines
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn at_main() -> GitState {
        GitState {
            commit: "4f2a1c9abcdef0123456789".to_string(),
            branch: Some("main".to_string()),
            dirty: false,
            at_origin_main: Some(true),
        }
    }

    #[test]
    fn a_clean_tree_at_origin_main_says_nothing() {
        assert!(warnings(&at_main()).is_empty());
    }

    /// The two conditions are independent, and a tree that fails both is told
    /// about both — the branch warning is about where the index will point,
    /// the dirty warning about what will be in it.
    #[test]
    fn both_conditions_are_reported_separately() {
        let state = GitState {
            branch: Some("feature/x".to_string()),
            dirty: true,
            at_origin_main: Some(false),
            ..at_main()
        };
        let lines = warnings(&state);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines[0],
            "This tree is at 4f2a1c9 on feature/x, not origin/main. The index will describe \
             this branch until the next CI scan."
        );
        assert_eq!(
            lines[1],
            "This tree has uncommitted changes. They will be indexed, and marked as such. \
             Cached analysis will not be kept for the changed files, and the next CI scan \
             re-analyses them."
        );
        assert_eq!(lines[2], LEAVE_ROUTINE_SCANS_TO_CI);
    }

    /// Warn, never refuse: the warning is a list of sentences and nothing
    /// about it can stop a run. Pinned because the ruling is the opposite of
    /// what a guard usually does.
    #[test]
    fn a_dirty_tree_at_origin_main_warns_about_the_tree_only() {
        let state = GitState {
            dirty: true,
            ..at_main()
        };
        let lines = warnings(&state);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("uncommitted changes"));
        assert!(!lines[0].contains("origin/main"));
    }

    /// A clone with no `origin/main` cannot say HEAD is away from it. Saying
    /// so anyway would warn every user whose default branch is named
    /// something else.
    #[test]
    fn an_unresolvable_origin_main_is_not_a_branch_warning() {
        let state = GitState {
            branch: Some("trunk".to_string()),
            at_origin_main: None,
            ..at_main()
        };
        assert!(warnings(&state).is_empty());
    }

    #[test]
    fn a_detached_head_is_named_as_one() {
        let state = GitState {
            branch: None,
            at_origin_main: Some(false),
            ..at_main()
        };
        assert!(warnings(&state)[0].contains("on a detached HEAD"));
    }

    /// The real thing, against a repository built for the test: a fresh commit
    /// with a modified file reads dirty, and the same tree reads clean once
    /// the change is committed.
    #[test]
    fn inspect_reads_a_real_repository() {
        let (repo, _head) = committed_repo(&[("a.txt", "one")]);
        let dir = repo.path();

        let clean = inspect(&dir.to_string_lossy());
        assert!(!clean.dirty, "a committed tree is clean: {clean:?}");
        assert_eq!(clean.branch.as_deref(), Some("main"));
        assert_eq!(clean.commit.len(), 40);
        // No remote in this repository, so the comparison cannot be made.
        assert_eq!(clean.at_origin_main, None);

        // carrick#2826: a file no scan reads leaves the tree clean.
        std::fs::write(dir.join("a.txt"), "two").unwrap();
        assert!(!inspect(&dir.to_string_lossy()).dirty);
        git_in(dir, &["checkout", "-q", "--", "a.txt"]);

        // A source file does not, edited or new.
        std::fs::write(dir.join("b.ts"), "export {};\n").unwrap();
        assert!(inspect(&dir.to_string_lossy()).dirty);
        git_in(dir, &["add", "b.ts"]);
        git_in(
            dir,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "b",
            ],
        );
        assert!(!inspect(&dir.to_string_lossy()).dirty);
        std::fs::write(dir.join("b.ts"), "export const b = 1;\n").unwrap();
        assert!(inspect(&dir.to_string_lossy()).dirty);
    }

    /// carrick#2826: dirty means a change to a file a scan reads. A test file,
    /// a README or an editor's settings is no such change; a manifest, a
    /// lockfile or a tsconfig is.
    #[test]
    fn only_a_change_to_a_file_a_scan_reads_is_dirty() {
        let (repo, _head) = committed_repo(&[("src/a.ts", "export {};\n")]);
        let dir = repo.path();
        let root = dir.to_string_lossy().into_owned();
        for file in [
            "README.md",
            ".vscode/settings.json",
            "src/a.test.ts",
            "docs/x.json",
        ] {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "{}").unwrap();
        }
        assert!(!inspect(&root).dirty, "nothing a scan reads changed");
        for file in ["package.json", "pnpm-lock.yaml", "tsconfig.base.json"] {
            std::fs::write(dir.join(file), "{}").unwrap();
            assert!(inspect(&root).dirty, "{file} is read by a scan");
            std::fs::remove_file(dir.join(file)).unwrap();
        }
    }

    /// What changed since a commit is listed from the top level whatever
    /// directory asks, against the working tree or another commit, and a
    /// commit the clone lacks is not an answer (carrick#2227).
    #[test]
    fn changed_paths_lists_the_whole_repository_from_its_top_level() {
        let (repo, first) = committed_repo(&[
            ("apps/api/a.ts", "a"),
            ("libs/util.ts", "u"),
            ("gone.ts", "g"),
        ]);
        let api = repo.path().join("apps/api");
        std::fs::write(repo.path().join("libs/util.ts"), "u2").unwrap();
        std::fs::remove_file(repo.path().join("gone.ts")).unwrap();
        std::fs::write(api.join("new.ts"), "n").unwrap();
        let mut changed = changed_paths(&api.to_string_lossy(), &first, None).unwrap();
        changed.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            changed,
            vec![
                ChangedPath {
                    status: 'A',
                    path: "apps/api/new.ts".to_string()
                },
                ChangedPath {
                    status: 'D',
                    path: "gone.ts".to_string()
                },
                ChangedPath {
                    status: 'M',
                    path: "libs/util.ts".to_string()
                },
            ]
        );

        git_in(repo.path(), &["add", "-A"]);
        git_in(repo.path(), &["commit", "-qm", "two"]);
        let second = git_in(repo.path(), &["rev-parse", "HEAD"]);
        let between = changed_paths(&api.to_string_lossy(), &first, Some(&second)).unwrap();
        assert_eq!(between.len(), 3, "{between:?}");
        assert!(
            changed_paths(&api.to_string_lossy(), &second, None)
                .unwrap()
                .is_empty()
        );
        assert!(changed_paths(&api.to_string_lossy(), &"f".repeat(40), None).is_err());
        assert!(changed_paths(&api.to_string_lossy(), "--output=x", None).is_err());
    }

    /// A throwaway repository on `main` with one commit holding `files`, and
    /// that commit's hash. The one git fixture for the whole crate: every test
    /// that asks git about a real tree builds its repository here, so the
    /// scoping the commands need — an ambient `GIT_DIR` removed, an author git
    /// will accept — is written once and cannot be half-remembered.
    ///
    /// The directory is a `TempDir`, so it is removed when the test drops it,
    /// including on a failed assertion.
    pub(crate) fn committed_repo(files: &[(&str, &str)]) -> (tempfile::TempDir, String) {
        let repo = tempfile::TempDir::new().unwrap();
        git_in(repo.path(), &["init", "-q", "-b", "main"]);
        for (relative, contents) in files {
            let path = repo.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        git_in(repo.path(), &["add", "-A"]);
        git_in(repo.path(), &["commit", "-qm", "one"]);
        let head = git_in(repo.path(), &["rev-parse", "HEAD"]);
        (repo, head)
    }

    /// One git command against `repo`, and its trimmed stdout.
    ///
    /// The environment is cleared of `GIT_DIR`, `GIT_WORK_TREE` and
    /// `GIT_INDEX_FILE` so the command is scoped to `repo` rather than to a
    /// repository an ambient variable names — a pre-commit hook running inside
    /// a worktree sets all three.
    pub(crate) fn git_in(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Everything the ruled first run writes before its scan, uncommitted,
    /// and nothing else: the tree is what the commit holds (carrick#1117).
    #[test]
    fn a_tree_whose_only_changes_are_the_scaffold_is_clean() {
        let (repo, _head) = committed_repo(&[
            ("AGENTS.md", "# Agents\n"),
            ("sub/index.ts", "export {};\n"),
        ]);
        let dir = repo.path();
        for file in [
            "carrick.json",
            ".github/workflows/carrick.yml",
            ".claude/skills/carrick/SKILL.md",
            ".claude/skills/carrick-impact/SKILL.md",
            ".claude/skills/carrick-reuse/SKILL.md",
            ".claude/skills/carrick-drift/SKILL.md",
            ".claude/skills/carrick-census/SKILL.md",
            ".agents/skills/carrick-impact/SKILL.md",
            ".agents/skills/carrick-reuse/SKILL.md",
            ".agents/skills/carrick-drift/SKILL.md",
            ".agents/skills/carrick-census/SKILL.md",
            ".claude/session-start.sh",
            ".claude/turn-reminder.sh",
            ".claude/search-gate.sh",
            ".claude/settings.json",
            // The Codex half of the same install (carrick#1335).
            ".codex/hooks.json",
            ".carrick/.gitignore",
            ".carrick/proposal.json",
        ] {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "{}").unwrap();
        }
        // The section the scaffold merges into an existing agent file.
        std::fs::write(dir.join("AGENTS.md"), "# Agents\n\n## Carrick\n").unwrap();
        let root = dir.to_string_lossy().into_owned();
        assert!(!inspect(&root).dirty, "scaffold only");

        // Staged as the agent's PR branch would have it: still no change to
        // anything the commit describes.
        git_in(dir, &["add", "carrick.json"]);
        assert!(!inspect(&root).dirty, "scaffold staged");

        // One real change beside it, and the tree is dirty again.
        std::fs::write(dir.join("sub").join("index.ts"), "export const a = 1;\n").unwrap();
        assert!(inspect(&root).dirty, "scaffold plus an edited source file");
    }

    /// An edit to a committed `carrick.json` changes what the scan reads, so
    /// it is a change; an untracked file that merely shares a scaffold name
    /// somewhere else is one only when a scan reads it.
    #[test]
    fn an_edited_committed_config_and_a_lookalike_path_still_count() {
        let (repo, _head) = committed_repo(&[
            ("AGENTS.md", "# Agents\n"),
            ("sub/index.ts", "export {};\n"),
        ]);
        let dir = repo.path();
        std::fs::write(dir.join("carrick.json"), "{}").unwrap();
        git_in(dir, &["add", "carrick.json"]);
        git_in(dir, &["commit", "-qm", "config"]);
        let root = dir.to_string_lossy().into_owned();
        assert!(!inspect(&root).dirty);

        std::fs::write(dir.join("carrick.json"), "{\"services\": []}").unwrap();
        assert!(inspect(&root).dirty, "an edited committed carrick.json");
        git_in(dir, &["checkout", "-q", "--", "carrick.json"]);

        // carrick#2826: a lookalike no scan reads is no change either way;
        // a lookalike that is source is.
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs").join("CLAUDE.md"), "x").unwrap();
        assert!(!inspect(&root).dirty, "a CLAUDE.md is not read by a scan");
        std::fs::create_dir_all(dir.join("sub/.claude")).unwrap();
        std::fs::write(dir.join("sub/.claude/settings.ts"), "export {};\n").unwrap();
        assert!(inspect(&root).dirty, "a source file under any name is read");
    }

    /// A scan root below the top level: git names the scaffold from the top
    /// (`sub/carrick.json`), and it is still the scaffold of the repo scanned.
    #[test]
    fn a_scan_root_inside_the_repository_finds_its_own_scaffold() {
        let (repo, _head) = committed_repo(&[
            ("AGENTS.md", "# Agents\n"),
            ("sub/index.ts", "export {};\n"),
        ]);
        let dir = repo.path();
        std::fs::write(dir.join("sub").join("carrick.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("sub").join(".claude")).unwrap();
        std::fs::write(dir.join("sub").join(".claude").join("settings.json"), "{}").unwrap();
        assert!(!inspect(&dir.join("sub").to_string_lossy()).dirty);
    }

    #[test]
    fn the_porcelain_answer_is_read_entry_by_entry() {
        assert!(!carries_changes_beyond_the_scaffold("", ""));
        assert!(!carries_changes_beyond_the_scaffold(
            "?? carrick.json\0 M AGENTS.md\0?? .carrick/scan-1.log\0",
            ""
        ));
        assert!(carries_changes_beyond_the_scaffold(
            "?? carrick.json\0 M src/a.ts\0",
            ""
        ));
        assert!(carries_changes_beyond_the_scaffold(" M carrick.json\0", ""));
        assert!(!carries_changes_beyond_the_scaffold(
            "AM carrick.json\0",
            ""
        ));
    }

    /// Every form git writes, read for the name and nothing else.
    ///
    /// The host is deliberately not a gate (carrick#1056): an ssh alias is a
    /// GitHub repository under a name only the user's ssh config resolves, and
    /// a name the workspace does not hold is refused by the cloud, which is
    /// the only side that can know. This is where the Rust reader differs from
    /// `repoIdentity` in the npm package, which resolves the alias through
    /// `ssh -G` because it decides what to OFFER the cloud.
    #[test]
    fn a_remote_names_its_repo_in_every_form_git_writes() {
        for (remote, expected) in [
            ("git@github.com:example/api.git", Some("example/api")),
            // The ticket's remote: a per-account ssh alias.
            (
                "git@github.com-personal:example/api.git",
                Some("example/api"),
            ),
            // The same alias as git writes it when the config names no user.
            ("github.com-personal:example/api.git", Some("example/api")),
            ("https://GitHub.COM/example/api.git", Some("example/api")),
            ("ssh://git@GitHub.COM/example/api.git", Some("example/api")),
            (
                "git+ssh://git@github.com/example/api.git",
                Some("example/api"),
            ),
            // A trailing slash is git's spelling, not a missing name.
            ("https://github.com/example/api/", Some("example/api")),
            // Another host entirely: it parses, and the cloud decides whether
            // it is a repository this workspace holds.
            ("https://gitlab.com/example/api.git", Some("example/api")),
            ("git@gitlab.com:group/sub/api.git", Some("sub/api")),
            // A credential in the URL is not part of the name.
            (
                "https://x-access-token:secret@github.com/example/api.git",
                Some("example/api"),
            ),
            // No `owner/repo` path to read.
            ("/tmp/api", None),
            ("https://github.com/example", None),
            ("git@github.com:api.git", None),
            ("file:///tmp/example/api.git", None),
            ("", None),
        ] {
            assert_eq!(parse_remote(remote).as_deref(), expected, "{remote}");
        }
    }

    /// The remote is printed when no name could be read from it, and a remote
    /// can carry a token. The redaction happens in the reader, before anything
    /// stores it.
    #[test]
    fn a_printed_remote_carries_no_credentials() {
        assert_eq!(
            without_credentials("https://x-access-token:secret@github.com/example"),
            "https://github.com/example"
        );
        assert!(!without_credentials("https://user:pw@host.test/").contains("pw"));
        // Nothing to strip: the string is returned exactly as git wrote it,
        // not as a URL parser would rewrite it.
        assert_eq!(
            without_credentials("git@github.com-personal:example/api.git"),
            "git@github.com-personal:example/api.git"
        );
        assert_eq!(
            without_credentials("https://github.com/example/api.git"),
            "https://github.com/example/api.git"
        );
    }

    /// The reader answers with the remote beside the name, so a surface can
    /// say "this remote names no repository" rather than "not connected".
    #[test]
    fn origin_reports_the_remote_it_could_not_name() {
        let dir = std::env::temp_dir().join(format!("carrick-git-origin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        // No origin at all: nothing to report.
        assert_eq!(origin(&dir), None);
        assert_eq!(remote_name(&dir), None);

        git(&[
            "remote",
            "add",
            "origin",
            "git@github.com-personal:example/api.git",
        ]);
        assert_eq!(
            origin(&dir),
            Some(Origin {
                url: "git@github.com-personal:example/api.git".to_string(),
                name: Some("example/api".to_string()),
            })
        );

        git(&["remote", "set-url", "origin", "/srv/mirrors/api"]);
        assert_eq!(
            origin(&dir),
            Some(Origin {
                url: "/srv/mirrors/api".to_string(),
                name: None,
            })
        );
        assert_eq!(remote_name(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path that is not a git repository answers with the default rather
    /// than a false claim about a branch.
    #[test]
    fn a_non_repository_reads_empty() {
        let dir = std::env::temp_dir().join(format!("carrick-git-none-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = inspect(&dir.to_string_lossy());
        assert_eq!(state, GitState::default());
        assert!(warnings(&state).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A runner's partial clone (`--filter=blob:none --depth=1`, then the
    /// stored commit fetched the same way) with the server moved away, so any
    /// lazy fetch fails loudly (carrick#2212). `a.txt` changed between the
    /// stored commit and HEAD, so the stored copy of it is not held; `b.txt`
    /// did not change. Returns the clone and the stored commit.
    pub(crate) fn partial_clone_without_remote(
        filter: bool,
    ) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let root = tempfile::TempDir::new().unwrap();
        let server = root.path().join("server");
        std::fs::create_dir_all(&server).unwrap();
        git_in(&server, &["init", "-q", "-b", "main"]);
        git_in(&server, &["config", "uploadpack.allowFilter", "true"]);
        git_in(
            &server,
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
        );
        std::fs::write(server.join("a.txt"), "one\n").unwrap();
        std::fs::write(server.join("b.txt"), "same\n").unwrap();
        git_in(&server, &["add", "-A"]);
        git_in(&server, &["commit", "-qm", "base"]);
        let base = git_in(&server, &["rev-parse", "HEAD"]);
        std::fs::write(server.join("a.txt"), "two\n").unwrap();
        git_in(&server, &["commit", "-qam", "head"]);

        let clone = clone_and_fetch(root.path(), &server, &base, filter);
        std::fs::rename(&server, root.path().join("server-gone")).unwrap();
        (root, clone, base)
    }

    /// A runner's partial clone of the repository at `server` as
    /// [`partial_clone_without_remote`] makes it, whose remote then names a
    /// path that does not exist. The clone is the returned directory's
    /// `clone`.
    pub(crate) fn clone_without_remote(
        server: &std::path::Path,
        stored: &str,
        filter: bool,
    ) -> tempfile::TempDir {
        git_in(server, &["config", "uploadpack.allowFilter", "true"]);
        git_in(server, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
        let root = tempfile::TempDir::new().unwrap();
        let clone = clone_and_fetch(root.path(), server, stored, filter);
        let gone = format!("file://{}", root.path().join("gone").display());
        git_in(&clone, &["remote", "set-url", "origin", &gone]);
        root
    }

    /// `server` cloned at depth 1 into `root/clone`, then `stored` fetched
    /// the same way, without blobs when `filter`.
    fn clone_and_fetch(
        root: &std::path::Path,
        server: &std::path::Path,
        stored: &str,
        filter: bool,
    ) -> std::path::PathBuf {
        let url = format!("file://{}", server.display());
        let clone = root.join("clone");
        let mut args = vec!["clone", "-q", "--depth=1"];
        if filter {
            args.push("--filter=blob:none");
        }
        args.extend([url.as_str(), clone.to_str().unwrap()]);
        git_in(root, &args);
        let mut fetch = vec!["fetch", "-q", "--depth=1"];
        if filter {
            fetch.push("--filter=blob:none");
        }
        fetch.extend(["origin", stored]);
        git_in(&clone, &fetch);
        clone
    }

    fn missing_objects(clone: &std::path::Path) -> usize {
        git_in(
            clone,
            &["rev-list", "--objects", "--missing=print", "--all"],
        )
        .lines()
        .filter(|line| line.starts_with('?'))
        .count()
    }

    #[test]
    fn a_clean_partial_clone_answers_without_fetching() {
        let (_root, clone, base) = partial_clone_without_remote(true);
        let before = missing_objects(&clone);
        let set = unchanged_since(clone.to_str().unwrap(), &base).unwrap();
        assert_eq!(set, HashSet::from(["b.txt".to_string()]));
        assert_eq!(missing_objects(&clone), before);
    }

    /// The rewritten-lockfile case: a tracked file edited after checkout,
    /// whose stored copy is absent, must read as changed, not fail the diff.
    #[test]
    fn an_edited_file_on_a_partial_clone_is_changed_not_a_failure() {
        let (_root, clone, base) = partial_clone_without_remote(true);
        let before = missing_objects(&clone);
        std::fs::write(clone.join("a.txt"), "edited after checkout\n").unwrap();
        let set = unchanged_since(clone.to_str().unwrap(), &base).unwrap();
        assert_eq!(set, HashSet::from(["b.txt".to_string()]));
        assert_eq!(missing_objects(&clone), before);
    }

    /// A full clone keeps git's default command and answers as before.
    #[test]
    fn an_edited_file_on_a_full_clone_is_changed() {
        let (_root, clone, base) = partial_clone_without_remote(false);
        std::fs::write(clone.join("a.txt"), "edited after checkout\n").unwrap();
        assert!(!is_partial_clone(clone.to_str().unwrap()));
        let set = unchanged_since(clone.to_str().unwrap(), &base).unwrap();
        assert_eq!(set, HashSet::from(["b.txt".to_string()]));
    }
}
