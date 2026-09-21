use indicatif::{ProgressBar, ProgressStyle};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tracing::info;
use tracing_appender::rolling;
use tracing_subscriber::fmt::writer::{BoxMakeWriter, MakeWriter};
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// The file this process's own lines go to, when it logs to a run file.
///
/// The run-log upload reads it whole. It used to read the day's shared file
/// from this process's start offset, and every carrick process on the machine
/// writes that file: a fifteen-minute scan shipped 118 other runs' banners
/// with its own, and nothing in a line said which process wrote it
/// (carrick#1133).
static RUN_LOG_FILE: OnceLock<PathBuf> = OnceLock::new();

/// Where a process's debug log goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSink {
    /// `~/.carrick/logs/carrick.log.<date>`, shared by every process that
    /// writes it that day: the builds `index` and `refresh` run, which upload
    /// nothing.
    Daily,
    /// `~/.carrick/logs/runs/<started>-<run>-<pid>.log`, this process's lines
    /// and nobody else's: the scan, which uploads its log (carrick#1133).
    Run,
}

/// UUID v4 generated once per scanner invocation. Sent on every cloud
/// request as `X-Carrick-Run-Id` and logged in the run preamble so the
/// same key joins customer-side and CloudWatch logs for one scan.
static RUN_ID: OnceLock<String> = OnceLock::new();

/// Set by a process that drives scans as subprocesses, so the whole build is
/// one run: `carrick index` runs a scan per repo and a join, and each of them
/// minting its own id printed a second "Carrick run starting" banner with a
/// different key and left the cloud's logs unable to join them (carrick#997
/// item 2). Internal, like [`crate::progress::PROGRESS_ENV`]; nothing asks a
/// user to set it.
pub const RUN_ID_ENV: &str = "CARRICK_RUN_ID";

/// What this process is inside that run, for its own banner. Set beside
/// [`RUN_ID_ENV`]; absent in the process a user started.
pub const RUN_PHASE_ENV: &str = "CARRICK_RUN_PHASE";

/// Stable identifier for this scanner run. Taken from the parent that drives
/// this one when there is one, else a fresh UUID v4 on first call, then the
/// same string for the rest of the process.
pub fn run_id() -> &'static str {
    RUN_ID.get_or_init(|| {
        std::env::var(RUN_ID_ENV)
            .ok()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    })
}

/// Initialize the global tracing subscriber with two layers:
///
/// 1. **Terminal layer** (stderr): Shows `INFO` by default, `DEBUG` with `--verbose`.
///    Uses a minimal format without timestamps or targets for a clean look.
///
/// 2. **File layer** (best effort): when `~/.carrick/logs/` is writable, appends
///    [`FILE_FILTER`] logs with timestamps to the file `sink` names — the
///    shared `carrick.log.YYYY-MM-DD` (daily rotation, [`RETAINED_LOG_DAYS`]
///    days kept), or a file of this process's own under `runs/`. If the
///    directory can't be created the file layer is skipped and only the
///    terminal layer is active — in that case the run preamble only reaches
///    stderr.
pub fn init(verbose: bool, sink: LogSink) {
    let terminal_filter = EnvFilter::new(terminal_filter(verbose));

    let terminal_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_level(false)
        .without_time()
        // Its own default, stated because the run depends on it: a terminal
        // that has gone away answers every write with an error, and a layer
        // that reported its own write errors would be doing it to the stream
        // that just failed — on the path a panicking print used to end runs on
        // (carrick#1386, and [`crate::console`] for the rest of it).
        .log_internal_errors(false)
        .with_filter(terminal_filter);

    // Try to set up file logging to ~/.carrick/logs/
    let log_dir = dirs::home_dir().map(|h| h.join(".carrick").join("logs"));

    let cap = log_size_cap();
    // Reported after the subscriber exists, so the fresh file says why it
    // starts where it does rather than a reader finding a log that begins
    // mid-scan.
    let mut rolled = None;
    let file_appender = log_dir.as_ref().and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        match sink {
            // Daily rotation with a retention cap. Rotation alone bounded
            // nothing — nothing deleted an old file, and 143 GB of them filled
            // a disk mid-session (carrick#741).
            LogSink::Daily => {
                // Roll before the size is read: a run that starts on a file
                // already at the cap starts on an empty one.
                rolled = roll_over_cap(dir, &today(), cap);
                let existing = std::fs::metadata(dir.join(format!("carrick.log.{}", today())))
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                let inner = rolling::Builder::new()
                    .rotation(rolling::Rotation::DAILY)
                    .filename_prefix("carrick.log")
                    .max_log_files(RETAINED_LOG_DAYS)
                    .build(dir)
                    .ok()?;
                Some(BoxMakeWriter::new(CappedAppender::new(
                    inner,
                    budget(cap, existing),
                    DAILY_LOG,
                )))
            }
            // A file nobody else writes, so what the upload reads is this
            // run's and only this run's (carrick#1133). Pruned here, on the
            // way in, by the same days as the daily files and a byte bound of
            // their size: a machine that runs a test suite starts hundreds of
            // scans a day, and the count is not what the disk cares about.
            LogSink::Run => {
                let runs = dir.join(RUN_LOG_DIR);
                std::fs::create_dir_all(&runs).ok()?;
                prune_run_logs(&runs, SystemTime::now(), run_logs_bound(cap));
                let path = runs.join(run_log_name(
                    run_id(),
                    std::process::id(),
                    chrono::Utc::now(),
                ));
                let file = create_private(&path).ok()?;
                let _ = RUN_LOG_FILE.set(path);
                Some(BoxMakeWriter::new(CappedAppender::new(
                    Mutex::new(file),
                    cap,
                    RUN_LOG,
                )))
            }
        }
    });

    if let Some(file_appender) = file_appender {
        let file_layer = fmt::layer()
            .with_writer(file_appender)
            .with_ansi(false)
            .with_target(true)
            .with_filter(EnvFilter::new(FILE_FILTER));

        let _ = tracing_subscriber::registry()
            .with(terminal_layer)
            .with(file_layer)
            .try_init();
        emit_run_preamble();
        if let Some(bytes) = rolled {
            info!(
                "Today's Carrick debug log had reached {:.0} MB (cap {} MB), so it was moved to \
                 carrick.log.<date>{} and this run starts a new one.",
                bytes as f64 / (1024.0 * 1024.0),
                cap / (1024 * 1024),
                ROLLED_SUFFIX
            );
        }
        warn_if_log_is_large();
        return;
    }

    // Fallback: terminal only
    let _ = tracing_subscriber::registry()
        .with(terminal_layer)
        .try_init();
    emit_run_preamble();
}

/// What the file layer writes: this crate at `debug`, everything else at
/// `info`.
///
/// It was a bare `debug`, which meant every dependency's debug lines too —
/// reqwest and hyper narrate each connection, its pool and its headers, and
/// those lines are the reason a laptop's log could not be shipped without
/// reading it first. Our own `debug` is what the file is for: stage names,
/// counts, timings and code identifiers. The terminal layer is unchanged; it
/// has its own filter and shows `info` (or `debug` with `--verbose`).
const FILE_FILTER: &str = "info,carrick=debug";

/// The target every per-attempt retry line is logged under.
///
/// One line per attempt is what a log file is for and what a terminal is not:
/// a scan under model pressure printed dozens of them, each naming transport
/// details (`Gateway status 429 with non-envelope body ...`) a package user
/// cannot act on (carrick#1103). The file keeps them at the level they were
/// logged at; the terminal gets one aggregated line instead
/// ([`crate::agent_service`]'s retry count), and `--verbose` shows them again.
pub const RETRY_TARGET: &str = "carrick::retry";

/// The target the run preamble is logged under.
///
/// The banner names the run id, the endpoint and eleven CI variables, which on
/// a laptop are eleven `<unset>`s. It is what a log file is read for and the
/// first thing a person running `carrick index` sees, before anything has
/// happened (carrick#1315). The file keeps it — every run-log reader joins on
/// it — and the terminal shows it only with `--verbose`.
pub const PREAMBLE_TARGET: &str = "carrick::runlog";

/// What the terminal layer shows: `info` (or `debug` with `--verbose`),
/// without the per-attempt retry lines or the run preamble unless verbose.
fn terminal_filter(verbose: bool) -> String {
    if verbose {
        "debug".to_string()
    } else {
        format!("info,{RETRY_TARGET}=off,{PREAMBLE_TARGET}=off")
    }
}

/// What a line may carry off the machine, and how it is rewritten until it
/// carries nothing else (carrick#1063, carrick#1098).
///
/// Shared by the run-log upload and the fail marker's `reason`, so a failure
/// and the log that explains it redact the same way. Built once per run: the
/// spellings below cost a `canonicalize` and a directory read each, and a log
/// is millions of lines.
///
/// The rules, in the order a line meets them:
///
/// 1. A line naming a credential goes entirely. `Authorization` is the header
///    this scanner sends, `X-Amz-Signature` rides every presigned URL it is
///    handed, and `carrick_sk_` is the literal prefix of the credential
///    itself. Whole lines rather than a surgical edit: a line naming a
///    credential is a line whose value is already suspect.
/// 2. The scanned repo's root becomes `<repo>` and the home directory `~`, in
///    every spelling this machine gives them: as the process holds them and
///    as `canonicalize` resolves them, because a symlinked or firmlinked home
///    is written the second way by everything that canonicalises a path.
///    Longest first, so a repo under the home keeps its repo-relative form.
/// 3. Any other absolute path on this machine keeps its last component and
///    loses the rest: `/private/tmp/x/api/src/a.ts` becomes `<path>/a.ts`. A
///    path is "on this machine" when its first segment is an entry of the
///    filesystem root, which is what tells `/tmp/...` from a route such as
///    `/api/users` without a list of directory names. A route whose first
///    segment happens to be a root entry (`/home`, `/dev`) loses its prefix
///    too, which costs a debug line some detail and never leaks a name.
/// 4. The account name, as a whole token (letters, digits and `_`, bounded by
///    anything else), becomes `<user>`. That is the backstop for a directory a
///    tool derived from the home path with `/` turned into `-`, which rule 2
///    cannot see because it is not the home path. Not applied on CI, where the
///    account (`runner`, `root`) names no developer and is an ordinary word.
///
/// Windows drive paths are not recognised by rule 3; rules 2 and 4 still
/// apply to them.
#[derive(Debug, Clone, Default)]
pub struct Redaction {
    /// Literal spellings and what each becomes, longest first.
    spellings: Vec<(String, &'static str)>,
    /// Account names, each at least two characters long.
    accounts: Vec<String>,
    /// The names directly under `/` on this machine.
    root_entries: std::collections::BTreeSet<String>,
}

const REPO_MARK: &str = "<repo>";
const HOME_MARK: &str = "~";
const PATH_MARK: &str = "<path>";
const USER_MARK: &str = "<user>";

impl Redaction {
    /// The redaction for this run on this machine, with `repo_path` as the
    /// scanned repo when there is one.
    pub fn for_run(repo_path: Option<&str>) -> Self {
        let spellings_of = |path: &Path| -> Vec<String> {
            let mut spellings = vec![path.to_string_lossy().into_owned()];
            if let Ok(canonical) = std::fs::canonicalize(path) {
                spellings.push(canonical.to_string_lossy().into_owned());
            }
            spellings
        };
        // `dirs::home_dir` is what `log_dir` already resolves the log
        // directory with, so the spelling this replaces is the one written.
        let home = dirs::home_dir();
        let homes = home.as_deref().map(spellings_of).unwrap_or_default();
        let repos = repo_path
            .map(|repo| spellings_of(Path::new(repo)))
            .unwrap_or_default();
        // A CI runner's account (`runner`, `root` in a container) names no
        // developer, and as a token it is an ordinary word: "the repo root"
        // would ship as "the repo <user>". Rules 2 and 3 still apply there.
        let mut accounts: Vec<String> = Vec::new();
        if std::env::var_os("CI").is_none() {
            accounts.extend(
                home.as_deref()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned()),
            );
            for variable in ["USER", "LOGNAME", "USERNAME"] {
                if let Ok(name) = std::env::var(variable) {
                    accounts.push(name);
                }
            }
        }
        let root_entries: Vec<String> = std::fs::read_dir("/")
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        Self::new(&repos, &homes, &accounts, &root_entries)
    }

    /// The same, from facts a test can state.
    pub fn new(
        repos: &[String],
        homes: &[String],
        accounts: &[String],
        root_entries: &[String],
    ) -> Self {
        let mut spellings: Vec<(String, &'static str)> = Vec::new();
        for (paths, mark) in [(repos, REPO_MARK), (homes, HOME_MARK)] {
            for path in paths {
                let path = path.trim_end_matches('/');
                // `/` as a home would turn every absolute path into `~...`,
                // and a one-character path is not one worth hiding.
                if path.len() > 1 && !spellings.iter().any(|(known, _)| known == path) {
                    spellings.push((path.to_string(), mark));
                }
            }
        }
        spellings.sort_by_key(|(spelling, _)| std::cmp::Reverse(spelling.len()));
        let mut kept_accounts: Vec<String> = Vec::new();
        for account in accounts {
            let account = account.trim();
            if account.chars().count() >= 2
                && account
                    .chars()
                    .all(|c| c.is_alphanumeric() || "._-".contains(c))
                && !kept_accounts.iter().any(|known| known == account)
            {
                kept_accounts.push(account.to_string());
            }
        }
        Self {
            spellings,
            accounts: kept_accounts,
            root_entries: root_entries
                .iter()
                .filter(|name| !name.is_empty())
                .cloned()
                .collect(),
        }
    }

    /// Rewrite one log line for upload, or drop it.
    pub fn line(&self, line: &str) -> Option<String> {
        const SECRET_MARKERS: [&str; 3] = ["Authorization", "X-Amz-Signature", "carrick_sk_"];
        if SECRET_MARKERS.iter().any(|marker| line.contains(marker)) {
            return None;
        }
        let mut text = line.to_string();
        for (spelling, mark) in &self.spellings {
            text = replace_path_prefix(&text, spelling, mark);
        }
        let text = self.redact_machine_paths(&text);
        Some(self.redact_accounts(&text))
    }

    /// [`Redaction::line`] over a whole log slice, dropping the lines it drops.
    pub fn log(&self, content: &str) -> String {
        let mut out = String::with_capacity(content.len());
        for line in content.lines() {
            if let Some(kept) = self.line(line) {
                out.push_str(&kept);
                out.push('\n');
            }
        }
        out
    }

    /// Rule 3: an absolute path whose first segment is a root entry keeps
    /// only its last component.
    fn redact_machine_paths(&self, text: &str) -> String {
        if self.root_entries.is_empty() {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < chars.len() {
            let starts_path = chars[i] == '/'
                && (i == 0 || !is_path_char(chars[i - 1]))
                && chars.get(i + 1).is_some_and(|c| *c != '/');
            if starts_path {
                let end = (i..chars.len())
                    .find(|&j| ends_path(chars[j]))
                    .unwrap_or(chars.len());
                let token: String = chars[i..end].iter().collect();
                let first = token[1..].split('/').next().unwrap_or_default();
                if self.root_entries.contains(first) {
                    let last = token.trim_end_matches('/').rsplit('/').next();
                    out.push_str(PATH_MARK);
                    if let Some(last) = last.filter(|last| *last != first) {
                        out.push('/');
                        out.push_str(last);
                    }
                    i = end;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    /// Rule 4: the account name as a whole alphanumeric token.
    fn redact_accounts(&self, text: &str) -> String {
        let mut text = text.to_string();
        for account in &self.accounts {
            let mut out = String::with_capacity(text.len());
            let mut rest = text.as_str();
            while let Some(at) = rest.find(account.as_str()) {
                let before = rest[..at].chars().next_back();
                let after = rest[at + account.len()..].chars().next();
                // `_` is part of a token, so `runner_os=Linux` in the run
                // preamble is not read as the account `runner`.
                let part_of_token = |c: char| c.is_alphanumeric() || c == '_';
                let whole = before.is_none_or(|c| !part_of_token(c))
                    && after.is_none_or(|c| !part_of_token(c));
                out.push_str(&rest[..at]);
                out.push_str(if whole { USER_MARK } else { account });
                rest = &rest[at + account.len()..];
            }
            out.push_str(rest);
            text = out;
        }
        text
    }
}

/// Replace `path` with `mark` wherever it appears as a whole path: followed by
/// a separator or by anything that is not part of a file name, so `/home/ada`
/// is not replaced inside `/home/adam`.
fn replace_path_prefix(text: &str, path: &str, mark: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(path) {
        let after = rest[at + path.len()..].chars().next();
        let whole = after.is_none_or(|c| c == '/' || !is_path_char(c));
        out.push_str(&rest[..at]);
        out.push_str(if whole { mark } else { path });
        rest = &rest[at + path.len()..];
    }
    out.push_str(rest);
    out
}

/// A character a file name or a path spelling can carry, for telling where a
/// path starts. `<` and `>` count, so a mark already written is never read as
/// the start of a new path.
fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || "._-~/<>@+%".contains(c)
}

/// Where an absolute path in a log line ends.
fn ends_path(c: char) -> bool {
    c.is_whitespace() || "\"'`,;:)]}>|".contains(c)
}

/// Bytes one debug log file may hold.
///
/// Days were the only bound until carrick#836, and days measure the wrong
/// thing: the file's size depends on how much anyone scanned that day, so an
/// ordinary day of local runs wrote 9.3 GB into one file and three days of
/// them was 25 GB on a laptop. Two hundred megabytes is far more DEBUG than
/// anyone reads, and with it the directory holds at most
/// [`RETAINED_LOG_DAYS`] files of the appender's plus the one generation this
/// module rolls aside: about 800 MB, whatever anyone scans.
///
/// The appender's own pruning matches on the `carrick.log` prefix, so whether
/// it counts a rolled `carrick.log.<date>.1` toward its cap depends on the
/// platform (it sorts by creation time where the filesystem reports one, and
/// falls back to parsing the date out of the name, which a rolled name defeats).
/// Either way the count is bounded, which is what the file names cannot be
/// relied on to say and what the test below asserts.
const LOG_SIZE_CAP_BYTES: u64 = 200 * 1024 * 1024;

/// Raise, lower or remove the byte cap for one run. `0` removes it, which is
/// what to set when a scan has to be debugged past 200 MB of its own output.
const LOG_SIZE_CAP_ENV: &str = "CARRICK_LOG_MAX_MB";

/// The suffix a rolled generation of today's file carries.
const ROLLED_SUFFIX: &str = ".1";

/// The byte cap this run writes under.
fn log_size_cap() -> u64 {
    match std::env::var(LOG_SIZE_CAP_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => u64::MAX,
            Ok(mb) => mb.saturating_mul(1024 * 1024),
            Err(_) => LOG_SIZE_CAP_BYTES,
        },
        Err(_) => LOG_SIZE_CAP_BYTES,
    }
}

/// What this run may still write to a file that already holds `existing`.
fn budget(cap: u64, existing: u64) -> u64 {
    if cap == u64::MAX {
        return u64::MAX;
    }
    cap.saturating_sub(existing)
}

/// The day this run's file is named for, in UTC.
///
/// One clock, and it is the appender's (carrick#1052). `tracing_appender`
/// names `carrick.log.<date>` from `Utc::now()` and nothing here can tell it
/// otherwise, so every reader of that name has to use the same clock or it
/// reads a file that does not exist. This is the reader: the roll at startup
/// and the size a run's budget is taken from both go through it. With `Local`
/// here, a machine west of UTC spent the hours between local
/// midnight and 00:00 UTC computing yesterday's name for today's file — the
/// roll looked at nothing, and the run-log upload found nothing to ship, which
/// is the window a first index was lost in.
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Move today's file aside when it is already at the cap, and keep exactly one
/// rolled generation in the directory. Returns the bytes moved.
///
/// The rolled name keeps the day in it (`carrick.log.<date>.1`) so a reader can
/// tell what it is, and every other rolled file goes: without that, a machine
/// scanning every day would accumulate one per day and the byte bound would be
/// a function of usage again, which is the bug.
///
/// `carrick index` runs one child process per repo and each one comes through
/// here, so a workspace whose repos write more than the cap between them rolls
/// mid-index and the earlier repos' lines go. At 200 MB a repo that reaches it
/// has already written more DEBUG than anyone reads; the run says when it
/// happened, and `CARRICK_LOG_MAX_MB=0` is there for the run that needs it all.
fn roll_over_cap(dir: &Path, day: &str, cap: u64) -> Option<u64> {
    let live = dir.join(format!("carrick.log.{day}"));
    // Built by hand rather than with `with_extension`, which would read the
    // date as the extension and write `carrick.log.1`.
    let rolled = dir.join(format!("carrick.log.{day}{ROLLED_SUFFIX}"));
    let size = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
    let over = size >= cap;
    for existing in rolled_files(dir) {
        // The one we are about to write is replaced; the rest are other days'.
        if !over && existing == rolled {
            continue;
        }
        let _ = std::fs::remove_file(existing);
    }
    if !over {
        return None;
    }
    std::fs::rename(&live, &rolled).ok().map(|()| size)
}

fn rolled_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("carrick.log.") && name.ends_with(ROLLED_SUFFIX)
                    })
        })
        .collect()
}

/// What the cap message calls the shared daily file.
const DAILY_LOG: &str = "Carrick's debug log for today";

/// What the cap message calls a run's own file.
const RUN_LOG: &str = "This run's debug log";

/// A log file's appender, with a byte budget for this run.
///
/// Rolling aside at startup bounds what earlier runs left behind; this bounds
/// what one run can write, which is the other half — a single large scan is
/// what produced the gigabytes. Past the budget the lines are dropped rather
/// than the run failing: a debug log is never worth ending a scan for. One
/// line on stderr says it happened and how to turn the cap off.
struct CappedAppender<A> {
    inner: A,
    remaining: AtomicU64,
    announced: AtomicBool,
    /// Which file this is, in the words of the cap message.
    what: &'static str,
}

impl<A> CappedAppender<A> {
    fn new(inner: A, remaining: u64, what: &'static str) -> Self {
        Self {
            inner,
            remaining: AtomicU64::new(remaining),
            announced: AtomicBool::new(false),
            what,
        }
    }
}

impl<'a, A: MakeWriter<'a>> MakeWriter<'a> for CappedAppender<A> {
    type Writer = CappedWriter<'a, A::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        CappedWriter {
            inner: self.inner.make_writer(),
            remaining: &self.remaining,
            announced: &self.announced,
            what: self.what,
        }
    }
}

struct CappedWriter<'a, W> {
    inner: W,
    remaining: &'a AtomicU64,
    announced: &'a AtomicBool,
    what: &'static str,
}

impl<W: Write> Write for CappedWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.remaining.load(Ordering::Relaxed);
        if left == 0 {
            // Reported as written: a dropped debug line is not an error the
            // scanner should see, and returning 0 would look like a full disk.
            return Ok(buf.len());
        }
        // A whole event is written or none of it is, so the file overshoots by
        // at most one line rather than ending mid-record.
        let written = self.inner.write(buf)?;
        let now = left.saturating_sub(written as u64);
        self.remaining.store(now, Ordering::Relaxed);
        if now == 0 && !self.announced.swap(true, Ordering::Relaxed) {
            crate::errln!(
                "{} reached its {} MB cap and the rest of this run is not being written to it. \
                 Set {}=0 to log without a cap.",
                self.what,
                log_size_cap() / (1024 * 1024),
                LOG_SIZE_CAP_ENV
            );
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// How many days of debug logs `~/.carrick/logs/` keeps.
///
/// The file layer writes at DEBUG whatever the terminal is set to, so a day
/// of scanning a large repo is measured in gigabytes and nothing but this cap
/// removes it: the appender's daily rotation only starts a new file. Three
/// days covers "what happened in the run I am asking about", which is all the
/// local copy is for — the cloud gets this run's slice at upload time.
const RETAINED_LOG_DAYS: usize = 3;

/// The directory under `~/.carrick/logs/` that holds one file per run.
///
/// A directory of its own, not a `carrick.log.*` name beside the daily files:
/// the appender prunes every file matching its prefix down to
/// [`RETAINED_LOG_DAYS`], so three scans named that way would evict every day
/// of the shared log, and the roll matches the same prefix.
const RUN_LOG_DIR: &str = "runs";

/// How long a run file that is not being written is protected from the byte
/// bound. A scan that is still going keeps writing, and deleting a file a
/// running scan still holds open would leave its upload reading nothing.
const RUN_LOG_IN_USE: Duration = Duration::from_secs(60 * 60);

/// The bytes every run file together may hold: the days the daily files are
/// kept for, at the size one of them is capped at. Unbounded when the cap is
/// off.
fn run_logs_bound(cap: u64) -> u64 {
    cap.saturating_mul(RETAINED_LOG_DAYS as u64)
}

/// `2026-09-15T13-28-04Z-e52ff358-41234.log`: when it started, the head of the
/// run id that joins it to the cloud's logs, and the process, because a build
/// and every scan it drives share one run id.
fn run_log_name(run_id: &str, pid: u32, started: chrono::DateTime<chrono::Utc>) -> String {
    let head: String = run_id.chars().take(8).collect();
    format!("{}-{head}-{pid}.log", started.format("%Y-%m-%dT%H-%M-%SZ"))
}

/// Create a run's log file readable by this account alone. It names the
/// machine's paths, which is why the upload redacts it; the local copy does
/// not need to be readable by anyone else to be useful.
fn create_private(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Delete the run files the retention no longer covers: every one older than
/// [`RETAINED_LOG_DAYS`], then the oldest of the rest while together they
/// hold more than `bound` bytes — except a file written in the last
/// [`RUN_LOG_IN_USE`], which may belong to a scan that is still running.
fn prune_run_logs(dir: &Path, now: SystemTime, bound: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let max_age = Duration::from_secs(60 * 60 * 24 * RETAINED_LOG_DAYS as u64);
    let mut kept: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("log") {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        let age = now.duration_since(modified).unwrap_or_default();
        if age > max_age {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        kept.push((path, modified, meta.len()));
    }
    let mut total: u64 = kept.iter().map(|(_, _, len)| len).sum();
    kept.sort_by_key(|(_, modified, _)| *modified);
    for (path, modified, len) in kept {
        if total <= bound {
            break;
        }
        if now.duration_since(modified).unwrap_or_default() < RUN_LOG_IN_USE {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Size at which the log directory is worth mentioning: nothing about a
/// scanner says "check your home directory", and the first symptom of not
/// knowing was a disk that filled mid-run (carrick#741). It measures the
/// directory rather than one file, because one file is now capped.
const LOG_SIZE_NOTICE_BYTES: u64 = 1024 * 1024 * 1024;

/// Say where the logs are and how much of the disk they hold, once that is
/// enough that someone would want to know. Silent below the threshold, which
/// is every ordinary run.
fn warn_if_log_is_large() {
    let Some(dir) = log_dir() else {
        return;
    };
    let size = log_dir_size(&dir);
    if size < LOG_SIZE_NOTICE_BYTES {
        return;
    }
    tracing::warn!(
        "Carrick's debug logs hold {:.1} GB ({}). Each file is capped at {} MB and {} day(s) are \
         kept; delete the directory to reclaim the space now.",
        size as f64 / (1024.0 * 1024.0 * 1024.0),
        dir.display(),
        log_size_cap() / (1024 * 1024),
        RETAINED_LOG_DAYS
    );
}

fn log_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".carrick").join("logs"))
}

/// The bytes the log directory holds, the run files under it included.
fn log_dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_file() => meta.len(),
            Ok(meta) if meta.is_dir() && entry.file_name() == RUN_LOG_DIR => {
                log_dir_size(&entry.path())
            }
            _ => 0,
        })
        .sum()
}

/// This process's own log file, when it writes one: the file the run-log
/// upload reads. `None` for a process that logs to the daily file, and for one
/// whose file could not be created.
pub fn run_log_file() -> Option<&'static Path> {
    RUN_LOG_FILE.get().map(PathBuf::as_path)
}

/// Emit a structured preamble at the start of every run. Goes through `tracing`
/// so it lands in the file log (when available) and the terminal (info+).
/// This is the "what was the environment when this ran" record that makes
/// uploaded logs interpretable after the fact.
///
/// Intentionally omits absolute filesystem paths (e.g. cwd) — this preamble is
/// uploaded to S3 from local runs as well as CI, and workstation paths often
/// contain usernames or internal directory names that aren't needed to
/// identify a run. GitHub repo/sha are sufficient for CI; for local runs the
/// repository name from the carrick.json + scanner version are enough.
fn emit_run_preamble() {
    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| "<unset>".to_string())
    }

    // A process a parent drives says which part of that parent's run it is, so
    // the second banner in a build's output is readable as the phase it
    // belongs to rather than as a second run (carrick#997 item 2).
    if let Ok(phase) = std::env::var(RUN_PHASE_ENV)
        && !phase.trim().is_empty()
    {
        info!(
            target: PREAMBLE_TARGET,
            run_id = run_id(),
            scanner_version = env!("CARGO_PKG_VERSION"),
            phase = %phase,
            "Carrick run continuing"
        );
        return;
    }

    info!(
        target: PREAMBLE_TARGET,
        run_id = run_id(),
        scanner_version = env!("CARGO_PKG_VERSION"),
        api_endpoint = env!("CARRICK_API_ENDPOINT"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        ci = %env("CI"),
        github_event = %env("GITHUB_EVENT_NAME"),
        github_ref = %env("GITHUB_REF"),
        github_repo = %env("GITHUB_REPOSITORY"),
        github_sha = %env("GITHUB_SHA"),
        github_run_id = %env("GITHUB_RUN_ID"),
        github_workflow = %env("GITHUB_WORKFLOW"),
        runner_os = %env("RUNNER_OS"),
        "Carrick run starting"
    );
}

/// True when stderr is attached to a terminal (TTY). False under CI redirects
/// like `./carrick . > analysis.log 2>&1`, where indicatif renders nothing
/// and the user sees a long silent gap between stage transitions.
pub fn is_tty() -> bool {
    std::io::stderr().is_terminal()
}

/// Create a spinner with a message. Call `finish_with_message` when done.
///
/// In non-TTY environments (CI, piped output) the animated spinner is
/// suppressed and a plain `▸ <msg>` line is emitted to stderr so the user
/// still sees the stage start. `finish_spinner` mirrors with `✓ <msg>`.
pub fn spinner(msg: &str) -> ProgressBar {
    if !is_tty() {
        crate::errln!("▸ {}", msg);
        // Returning a hidden ProgressBar keeps the call sites uniform — no
        // ticks are drawn and `finish_*` skips its rendering path. The
        // matching `✓ <msg>` line is still emitted by the same is_tty()
        // gate inside `finish_spinner`.
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Say which item a running stage has reached, without finishing it.
///
/// A stage that loops over services is one spinner for the whole loop, and a
/// thirteen-service repo spent minutes on each without saying which
/// (carrick#1067). In a terminal that is the spinner's own label; in a pipe —
/// which is where a detached scan's log is read — it is one `▸ <msg>` line per
/// item, the same shape [`spinner`] emits for a stage start.
pub fn progress(pb: &ProgressBar, msg: &str) {
    if !is_tty() {
        crate::errln!("▸ {}", msg);
        return;
    }
    pb.set_message(msg.to_string());
}

/// Finish a spinner with a success checkmark.
pub fn finish_spinner(pb: &ProgressBar, msg: &str) {
    if !is_tty() {
        crate::errln!("✓ {}", msg);
        return;
    }
    pb.set_style(ProgressStyle::with_template("  {msg}").unwrap());
    pb.finish_with_message(format!("\x1b[32m✓\x1b[0m {}", msg));
}

/// Severity of a GitHub Actions annotation.
pub enum Annotation {
    Warning,
    Error,
}

/// Emit a GitHub Actions annotation for `msg`, when running in one.
///
/// The Action tees the scanner's own output and nothing else, so a line the
/// scanner does not print is a line the Action log does not carry. An
/// annotation additionally surfaces on the run summary and next to the step,
/// which is where someone looks when a scan finished but did less than it was
/// asked to. Outside Actions this prints nothing: the same fact reaches a
/// local run through the warning it accompanies.
pub fn annotate(level: Annotation, msg: &str) {
    if std::env::var("GITHUB_ACTIONS").is_err() {
        return;
    }
    let tag = match level {
        Annotation::Warning => "warning",
        Annotation::Error => "error",
    };
    // Annotations are one line: a newline would end the command and print the
    // rest as ordinary log text.
    crate::errln!("::{}::{}", tag, msg.replace('\n', " "));
}

/// Finish a spinner with a warning marker.
pub fn finish_spinner_warn(pb: &ProgressBar, msg: &str) {
    if !is_tty() {
        crate::errln!("⚠ {}", msg);
        return;
    }
    pb.set_style(ProgressStyle::with_template("  {msg}").unwrap());
    pb.finish_with_message(format!("\x1b[33m⚠\x1b[0m {}", msg));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    /// The retention cap must not change the file name.
    ///
    /// The roll at startup computes `carrick.log.<date>` by hand, and the
    /// run's byte budget is taken from the size of that exact file. A builder
    /// that spelled the name differently would leave both looking at nothing,
    /// and the cap would stop capping without failing.
    #[test]
    fn the_capped_appender_writes_the_name_the_roll_looks_for() {
        let dir = tempfile::tempdir().expect("temp dir");

        let mut appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir.path())
            .expect("build appender");
        writeln!(appender, "a line").expect("write");
        appender.flush().expect("flush");

        // `today()`, not a second spelling of the clock: the point of the
        // assertion is that the name the roll computes is the name the
        // appender wrote (carrick#1052).
        let expected = dir.path().join(format!("carrick.log.{}", today()));
        assert!(
            expected.is_file(),
            "expected {}, found {:?}",
            expected.display(),
            log_files(dir.path())
        );
    }

    /// The cap must delete, and it must do so on an ordinary run.
    ///
    /// If it only pruned when the clock crossed midnight, a machine already
    /// holding six days of logs would keep holding them however many scans it
    /// ran, which is the case carrick#741 was reported from.
    #[test]
    fn the_cap_deletes_the_days_it_is_past_on_the_first_write() {
        let dir = tempfile::tempdir().expect("temp dir");
        for day in ["2026-09-01", "2026-09-02", "2026-09-03", "2026-09-04"] {
            std::fs::write(dir.path().join(format!("carrick.log.{day}")), "old\n")
                .expect("seed old log");
        }

        let mut appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir.path())
            .expect("build appender");
        writeln!(appender, "a line").expect("write");
        appender.flush().expect("flush");

        let kept = log_files(dir.path());
        assert!(
            kept.len() <= RETAINED_LOG_DAYS,
            "kept {} files, cap is {}: {:?}",
            kept.len(),
            RETAINED_LOG_DAYS,
            kept
        );
        assert!(
            kept.contains(&format!("carrick.log.{}", today())),
            "today's file was pruned: {kept:?}"
        );
        assert!(
            !kept.contains(&"carrick.log.2026-09-01".to_string()),
            "the oldest file survived: {kept:?}"
        );
    }

    /// A file already at the cap is moved aside, so this run logs to an empty
    /// one. The bytes that filled it were earlier runs', and they are the ones
    /// worth losing.
    #[test]
    fn a_file_at_the_cap_is_rolled_aside_before_the_run_writes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let live = dir.path().join("carrick.log.2026-09-08");
        std::fs::write(&live, vec![b'x'; 512]).expect("seed");

        roll_over_cap(dir.path(), "2026-09-08", 256);

        assert!(
            !live.exists(),
            "today's file was left in place: {:?}",
            log_files(dir.path())
        );
        assert_eq!(
            std::fs::read(dir.path().join("carrick.log.2026-09-08.1"))
                .expect("rolled file")
                .len(),
            512
        );
    }

    /// Under the cap nothing moves: an ordinary run must not lose the log of
    /// the run before it.
    #[test]
    fn a_file_under_the_cap_is_left_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let live = dir.path().join("carrick.log.2026-09-08");
        std::fs::write(&live, vec![b'x'; 16]).expect("seed");

        assert_eq!(roll_over_cap(dir.path(), "2026-09-08", 256), None);

        assert_eq!(std::fs::read(&live).expect("today's file").len(), 16);
        assert!(
            log_files(dir.path())
                .iter()
                .all(|name| !name.ends_with(".1"))
        );
    }

    /// One rolled generation exists at a time. Keeping one per day would make
    /// the byte bound a function of how many days the machine scanned on,
    /// which is the thing carrick#836 is about.
    #[test]
    fn only_one_rolled_generation_survives_a_roll() {
        let dir = tempfile::tempdir().expect("temp dir");
        for day in ["2026-09-05", "2026-09-06", "2026-09-07"] {
            std::fs::write(dir.path().join(format!("carrick.log.{day}.1")), "old\n")
                .expect("seed rolled");
        }
        std::fs::write(dir.path().join("carrick.log.2026-09-08"), vec![b'x'; 512])
            .expect("seed today");

        roll_over_cap(dir.path(), "2026-09-08", 256);

        let rolled: Vec<String> = log_files(dir.path())
            .into_iter()
            .filter(|name| name.ends_with(".1"))
            .collect();
        assert_eq!(rolled, vec!["carrick.log.2026-09-08.1".to_string()]);
    }

    /// The budget is what a run may add, not what the file may hold, so a file
    /// already part-full does not get a second full cap written into it.
    #[test]
    fn the_budget_is_the_cap_less_what_is_already_there() {
        assert_eq!(budget(1000, 0), 1000);
        assert_eq!(budget(1000, 400), 600);
        assert_eq!(budget(1000, 4000), 0);
        assert_eq!(budget(u64::MAX, 4000), u64::MAX);
    }

    /// Past the budget the writer drops lines and reports them written: a
    /// debug log is never worth failing a scan for, and a short write would
    /// read as a full disk.
    #[test]
    fn the_writer_stops_at_the_budget_without_failing_the_run() {
        let dir = tempfile::tempdir().expect("temp dir");
        let appender = CappedAppender::new(
            rolling::Builder::new()
                .rotation(rolling::Rotation::DAILY)
                .filename_prefix("carrick.log")
                .max_log_files(RETAINED_LOG_DAYS)
                .build(dir.path())
                .expect("build appender"),
            10,
            DAILY_LOG,
        );

        let line = b"0123456789abcdef\n";
        for _ in 0..5 {
            let mut writer = appender.make_writer();
            assert_eq!(writer.write(line).expect("write"), line.len());
            writer.flush().expect("flush");
        }

        let written = std::fs::metadata(dir.path().join(format!("carrick.log.{}", today())))
            .expect("today's file")
            .len();
        // One event of overshoot is allowed; five are not.
        assert_eq!(written, line.len() as u64);
    }

    /// A run's file is named for when it started, the run it belongs to and
    /// the process that wrote it: a build and each scan it drives share a run
    /// id, and each of them is a file.
    #[test]
    fn a_run_file_names_its_start_its_run_and_its_process() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-09-15T13:28:04Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            run_log_name("e52ff358-1c2d-4f7e-9a0b-5d6e7f8a9b0c", 41234, started),
            "2026-09-15T13-28-04Z-e52ff358-41234.log"
        );
    }

    /// A run file is written by one process, capped like the daily file, and
    /// readable by this account alone (carrick#1133).
    #[test]
    fn a_run_file_is_private_and_capped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("run.log");
        let appender = CappedAppender::new(
            Mutex::new(create_private(&path).expect("create")),
            10,
            RUN_LOG,
        );
        let line = b"0123456789abcdef\n";
        for _ in 0..3 {
            let mut writer = appender.make_writer();
            assert_eq!(writer.write(line).expect("write"), line.len());
        }
        assert_eq!(std::fs::read(&path).expect("read").len(), line.len());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
    }

    /// The run files are kept for the days the daily files are, and together
    /// they hold no more than their byte bound — but a file a scan may still
    /// be writing is never the one that goes (carrick#1133).
    #[test]
    fn run_files_are_pruned_by_age_and_then_by_bytes_oldest_first() {
        let dir = tempfile::tempdir().expect("temp dir");
        let now = SystemTime::now();
        let hours = |h: u64| now - Duration::from_secs(h * 60 * 60);
        let seed = |name: &str, bytes: usize, modified: SystemTime| {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![b'x'; bytes]).expect("seed");
            std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("open")
                .set_modified(modified)
                .expect("set mtime");
        };
        // Past the days kept, however small.
        seed("expired.log", 1, hours(24 * RETAINED_LOG_DAYS as u64 + 1));
        // Within the days, and together over a bound of 250 bytes.
        seed("oldest.log", 100, hours(30));
        seed("older.log", 100, hours(20));
        seed("recent.log", 100, hours(2));
        // Written a minute ago: may be a running scan's, so it stays even
        // though the bound is still exceeded without it.
        seed("live.log", 100, now - Duration::from_secs(60));
        // Not a run file.
        seed("notes.txt", 1_000, hours(24 * 30));

        prune_run_logs(dir.path(), now, 250);

        assert_eq!(
            log_files(dir.path()),
            vec![
                "live.log".to_string(),
                "notes.txt".to_string(),
                "recent.log".to_string()
            ]
        );
    }

    /// The directory's size counts the run files, so the notice about a large
    /// log directory still fires when they are what is large.
    #[test]
    fn the_log_directory_size_counts_the_run_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("carrick.log.2026-09-15"), vec![b'x'; 10]).unwrap();
        std::fs::create_dir_all(dir.path().join(RUN_LOG_DIR)).unwrap();
        std::fs::write(dir.path().join(RUN_LOG_DIR).join("a.log"), vec![b'x'; 32]).unwrap();
        assert_eq!(log_dir_size(dir.path()), 42);
        assert_eq!(run_logs_bound(u64::MAX), u64::MAX);
    }

    /// The cap is a number a user can change for one run, including off.
    #[test]
    fn the_cap_reads_the_environment() {
        // Parsed by the same function the run uses; the env var itself is
        // process-global, so this checks the arithmetic it does with it.
        assert_eq!(LOG_SIZE_CAP_BYTES, 200 * 1024 * 1024);
        assert_eq!(budget(log_size_cap(), 0), LOG_SIZE_CAP_BYTES);
    }

    /// The two prunings together leave a bounded number of files, whichever
    /// of them counts the rolled generation on this platform. The appender
    /// sorts by creation time where the filesystem has one and by the date in
    /// the name where it does not, and a rolled name has no parseable date —
    /// so the count, not the naming, is the thing to assert.
    #[test]
    fn a_roll_never_lifts_the_file_count_above_the_cap_plus_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        for day in ["2026-09-01", "2026-09-02", "2026-09-03", "2026-09-04"] {
            std::fs::write(dir.path().join(format!("carrick.log.{day}")), "old\n")
                .expect("seed old log");
        }
        std::fs::write(dir.path().join("carrick.log.2026-09-01.1"), "older still\n")
            .expect("seed rolled");
        // The appender's clock, read once and used for both the seed and the
        // roll, so the two cannot disagree across a midnight (carrick#1052).
        let day = today();
        assert_eq!(day, chrono::Utc::now().format("%Y-%m-%d").to_string());
        std::fs::write(
            dir.path().join(format!("carrick.log.{day}")),
            vec![b'x'; 512],
        )
        .expect("seed today");

        assert_eq!(roll_over_cap(dir.path(), &day, 256), Some(512));

        let mut appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir.path())
            .expect("build appender");
        writeln!(appender, "a line").expect("write");
        appender.flush().expect("flush");

        let kept = log_files(dir.path());
        assert!(
            kept.len() <= RETAINED_LOG_DAYS + 1,
            "kept {} files, bound is {}: {kept:?}",
            kept.len(),
            RETAINED_LOG_DAYS + 1
        );
        assert!(
            kept.contains(&format!("carrick.log.{day}")),
            "today's file is not there: {kept:?}"
        );
        assert!(
            kept.iter()
                .filter(|name| name.ends_with(ROLLED_SUFFIX))
                .count()
                <= 1,
            "more than one rolled generation: {kept:?}"
        );
    }

    /// One clock, and it is the appender's (carrick#1052).
    ///
    /// `tracing_appender` names its files from `Utc::now()`. Everything that
    /// reads that name — the roll at startup, the size a run's budget is taken
    /// from — goes through `today()`, so this is the assertion that keeps them
    /// on the same day. With `Local` here, every machine west of UTC had a
    /// window between local midnight and 00:00 UTC in which the roll looked at
    /// a file nobody was writing.
    #[test]
    fn todays_name_is_computed_on_the_appenders_clock() {
        assert_eq!(today(), chrono::Utc::now().format("%Y-%m-%d").to_string());

        // And the appender agrees, which is the half a constant cannot state.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("carrick.log")
            .max_log_files(RETAINED_LOG_DAYS)
            .build(dir.path())
            .expect("build appender");
        writeln!(appender, "a line").expect("write");
        appender.flush().expect("flush");
        assert!(
            dir.path()
                .join(format!("carrick.log.{}", today()))
                .is_file(),
            "the appender wrote {:?}, not carrick.log.{}",
            log_files(dir.path()),
            today()
        );
    }

    /// The file layer keeps our own debug lines and drops everyone else's.
    ///
    /// The filter is the whole reason a laptop's log can be shipped: at a bare
    /// `debug` the file carried reqwest's and hyper's per-connection narration,
    /// which is where URLs, headers and pool internals appear. Driven through a
    /// real subscriber rather than by reading the directive string, so a filter
    /// that stops meaning what it says fails here.
    #[test]
    fn the_file_layer_keeps_our_debug_and_nobody_elses() {
        let written = Shared::default();
        let layer = fmt::layer()
            .with_writer(written.clone())
            .with_ansi(false)
            .with_target(true)
            .with_filter(EnvFilter::new(FILE_FILTER));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "carrick::engine", "ours at debug");
            tracing::debug!(target: "reqwest::connect", "theirs at debug");
            tracing::debug!(target: "hyper_util::client::legacy::pool", "theirs too");
            // Another crate's info is still worth keeping: it is rare, and a
            // warning from a dependency is often the only record of why a run
            // behaved the way it did.
            tracing::info!(target: "reqwest::connect", "theirs at info");
        });

        let log = written.text();
        assert!(log.contains("ours at debug"), "{log}");
        assert!(log.contains("theirs at info"), "{log}");
        assert!(!log.contains("theirs at debug"), "{log}");
        assert!(!log.contains("theirs too"), "{log}");
    }

    /// A per-attempt retry line reaches the file at warn and never the
    /// terminal, unless the run is verbose (carrick#1103).
    #[test]
    fn retry_lines_go_to_the_file_and_not_the_terminal() {
        let terminal = Shared::default();
        let verbose_terminal = Shared::default();
        let file = Shared::default();
        let subscriber = tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_writer(terminal.clone())
                    .with_ansi(false)
                    .with_filter(EnvFilter::new(terminal_filter(false))),
            )
            .with(
                fmt::layer()
                    .with_writer(verbose_terminal.clone())
                    .with_ansi(false)
                    .with_filter(EnvFilter::new(terminal_filter(true))),
            )
            .with(
                fmt::layer()
                    .with_writer(file.clone())
                    .with_ansi(false)
                    .with_target(true)
                    .with_filter(EnvFilter::new(FILE_FILTER)),
            );

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: RETRY_TARGET, "Gateway status 429, attempt 2/5");
            tracing::info!(target: "carrick::agent_service", "Carrick Cloud did not answer, retrying (1 so far)");
        });

        let terminal = terminal.text();
        assert!(!terminal.contains("attempt 2/5"), "{terminal}");
        assert!(terminal.contains("retrying (1 so far)"), "{terminal}");
        assert!(verbose_terminal.text().contains("attempt 2/5"));
        let file = file.text();
        assert!(
            file.contains("WARN") && file.contains("attempt 2/5"),
            "{file}"
        );
    }

    /// The run preamble reaches the file always and the terminal only when
    /// the run is verbose (carrick#1315).
    ///
    /// It is the first thing `carrick index` printed, before anything had
    /// happened, and on a laptop eleven of its fields are `<unset>`. Every
    /// run-log reader joins on it, so the file must keep it.
    #[test]
    fn the_run_preamble_goes_to_the_file_and_not_the_terminal() {
        let terminal = Shared::default();
        let verbose_terminal = Shared::default();
        let file = Shared::default();
        let subscriber = tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_writer(terminal.clone())
                    .with_ansi(false)
                    .with_filter(EnvFilter::new(terminal_filter(false))),
            )
            .with(
                fmt::layer()
                    .with_writer(verbose_terminal.clone())
                    .with_ansi(false)
                    .with_filter(EnvFilter::new(terminal_filter(true))),
            )
            .with(
                fmt::layer()
                    .with_writer(file.clone())
                    .with_ansi(false)
                    .with_target(true)
                    .with_filter(EnvFilter::new(FILE_FILTER)),
            );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: PREAMBLE_TARGET, ci = "<unset>", "Carrick run starting");
            tracing::info!(target: PREAMBLE_TARGET, "Carrick run continuing");
            tracing::info!(target: "carrick::local_mode", "indexed api");
        });

        let terminal = terminal.text();
        assert!(!terminal.contains("Carrick run starting"), "{terminal}");
        assert!(!terminal.contains("Carrick run continuing"), "{terminal}");
        assert!(terminal.contains("indexed api"), "{terminal}");
        let verbose = verbose_terminal.text();
        assert!(verbose.contains("Carrick run starting"), "{verbose}");
        let file = file.text();
        assert!(file.contains("Carrick run starting"), "{file}");
        assert!(file.contains("Carrick run continuing"), "{file}");
    }

    /// A writer the test can read back.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Shared {
        type Writer = Shared;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// A laptop's paths name the account and often the employer. The repo and
    /// the scanner version identify the run; the home directory identifies the
    /// person.
    #[test]
    fn the_home_directory_becomes_a_tilde() {
        let redaction = ada(&[]);
        assert_eq!(
            redaction
                .line("  reading /Users/ada/work/payments-api/src/index.ts")
                .as_deref(),
            Some("  reading ~/work/payments-api/src/index.ts")
        );
        // Several times on one line, because that is how a log writes them,
        // and not inside a longer name that merely starts the same way.
        assert_eq!(
            redaction.line("/Users/ada/a -> /Users/ada/b").as_deref(),
            Some("~/a -> ~/b")
        );
        assert_eq!(redaction.line("/Users/adam/a").as_deref(), Some("<path>/a"));
        // Nothing to redact against is not an error; the line ships as it is.
        assert_eq!(
            Redaction::default().line("/Users/ada/a").as_deref(),
            Some("/Users/ada/a")
        );
    }

    /// The machine facts the redaction tests state: an account called `ada`,
    /// her home in both of the spellings macOS gives it, and a root directory
    /// with the entries a Mac has.
    fn ada(repos: &[&str]) -> Redaction {
        let owned = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Redaction::new(
            &owned(repos),
            &owned(&["/Users/ada", "/System/Volumes/Data/Users/ada"]),
            &owned(&["ada"]),
            &owned(&["Users", "System", "private", "tmp", "var", "Volumes", "opt"]),
        )
    }

    /// carrick#1098: a checkout outside the home directory, under a directory
    /// a tool derived from the home path, shipped the account name on every
    /// line that named a file in it.
    #[test]
    fn a_checkout_outside_home_names_neither_the_account_nor_the_machine() {
        let repo = "/private/tmp/claude-501/-Users-ada-Repositories-carrick/scratch/api";
        let redaction = ada(&[repo]);
        let line = format!(
            "Failed to parse {repo}/src/index.ts: Expected ';' (see also \
             /private/tmp/claude-501/-Users-ada-Repositories-carrick/other/x.ts:12:3)"
        );
        let redacted = redaction.line(&line).expect("kept");
        assert_eq!(
            redacted,
            "Failed to parse <repo>/src/index.ts: Expected ';' (see also <path>/x.ts:12:3)"
        );
        assert!(!redacted.contains("ada"), "{redacted}");
    }

    /// A home reached through its canonical spelling is still the home, and a
    /// repo under it keeps its repo-relative form rather than a home-relative
    /// one.
    #[test]
    fn every_spelling_of_the_home_and_the_repo_is_redacted_longest_first() {
        let redaction = ada(&["/Users/ada/work/api"]);
        assert_eq!(
            redaction
                .line("tsconfig at /System/Volumes/Data/Users/ada/.config/x.json")
                .as_deref(),
            Some("tsconfig at ~/.config/x.json")
        );
        assert_eq!(
            redaction
                .line("reading /Users/ada/work/api/src/a.ts")
                .as_deref(),
            Some("reading <repo>/src/a.ts")
        );
    }

    /// The account name goes wherever it is a whole token, and nowhere else.
    #[test]
    fn the_account_name_is_redacted_as_a_token_and_only_as_one() {
        let redaction = ada(&[]);
        assert_eq!(
            redaction
                .line("cache key -Users-ada-Repositories-carrick user=ada")
                .as_deref(),
            Some("cache key -Users-<user>-Repositories-carrick user=<user>")
        );
        assert_eq!(
            redaction.line("reading the adapter for canada").as_deref(),
            Some("reading the adapter for canada")
        );
        // An underscore joins a token: a field name that starts with the
        // account is not the account.
        let runner = Redaction::new(&[], &[], &["runner".to_string()], &[]);
        assert_eq!(
            runner.line("runner_os=Linux user=runner").as_deref(),
            Some("runner_os=Linux user=<user>")
        );
    }

    /// On CI the account names no developer and is an ordinary word, so the
    /// run's own redaction states no account at all.
    #[test]
    #[serial_test::serial]
    fn a_ci_run_redacts_no_account_name() {
        let previous = std::env::var_os("CI");
        // SAFETY: a `#[serial]` test, and the variable is restored below.
        unsafe { std::env::set_var("CI", "true") };
        let on_ci = Redaction::for_run(None);
        unsafe {
            match previous {
                Some(value) => std::env::set_var("CI", value),
                None => std::env::remove_var("CI"),
            }
        }
        assert!(on_ci.accounts.is_empty(), "{on_ci:?}");
        assert_eq!(
            on_ci
                .line("Discovered 3 file(s) under the repo root")
                .as_deref(),
            Some("Discovered 3 file(s) under the repo root")
        );
    }

    /// A route, a URL and a relative path are not machine paths: their first
    /// segment is not an entry of the filesystem root.
    #[test]
    fn routes_urls_and_relative_paths_are_left_alone() {
        let redaction = ada(&[]);
        for line in [
            "GET /api/users/:id matched POST /orders",
            "calling https://api.carrick.tools/mcp",
            "reading src/tmp/a.ts and ./tmp/b.ts",
        ] {
            assert_eq!(redaction.line(line).as_deref(), Some(line), "{line}");
        }
        assert_eq!(
            redaction.line("wrote /var/folders/x/T/out.json").as_deref(),
            Some("wrote <path>/out.json")
        );
    }

    /// A line naming a credential does not leave the machine, whichever of the
    /// three shapes it names it in. None of these are logged today — the rule
    /// is what keeps a line added later from shipping one.
    #[test]
    fn a_line_naming_a_credential_is_dropped_whole() {
        for line in [
            "  DEBUG request headers: Authorization: Bearer abc",
            "  DEBUG presigned PUT https://b.s3/k?X-Amz-Signature=deadbeef",
            "  DEBUG credential carrick_sk_live_9f2 loaded",
        ] {
            assert_eq!(ada(&[]).line(line), None, "not dropped: {line}");
        }
    }

    /// The rules over a slice: the credential line goes, the rest keeps its
    /// order and loses its home directory.
    #[test]
    fn redacting_a_slice_drops_lines_and_rewrites_the_rest() {
        let redacted = ada(&[]).log("first /Users/ada/a\n  Authorization: Bearer abc\nlast line\n");
        assert_eq!(redacted, "first ~/a\nlast line\n");
    }

    fn log_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}
