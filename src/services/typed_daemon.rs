//! The resident type daemon, from the CLI's side (carrick#2884).
//!
//! The sidecar's server mode (`node dist/src/index.js --serve <workspace>`,
//! carrick#2879, documented in `src/sidecar/README.md`, "Server mode") holds
//! each service's TypeScript program between runs and answers again only what
//! an edit reaches (carrick#2880, carrick#2881). This module is everything the
//! Rust side needs to use one:
//!
//! - [`build_id`]: the build id of the sidecar a scan would spawn, the same
//!   hash the sidecar's `buildIdOf` takes, so a daemon of another build is
//!   never asked;
//! - [`probe`]: whether a daemon serves a workspace, on which build, and which
//!   services it holds a built program for;
//! - [`start_detached`], [`retire`] and [`stop`]: its lifecycle. It is started
//!   by the CLI, lazily, as a process of the user's own, never as a system
//!   service, and signalled only once its identity is proven;
//! - [`backed_off`]: a build that crashed twice within an hour is not started
//!   again for a day;
//! - [`Use`]: what a `check` hands the scan it starts, so the scan's services
//!   connect to the daemon instead of each spawning a process
//!   ([`crate::services::type_sidecar::SidecarRecipe::with_daemon`]).
//!
//! When to start, use or warm it is `local_mode::typed`'s. The rules are the
//! lifecycle policy (L0-L6) in the sidecar README's "Server mode" section.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Turns the daemon off for this process and every scan it starts: `off`,
/// `0` or `false`. Every `check` then answers as it did before the daemon
/// existed, and none is started.
pub const OFF_ENV: &str = "CARRICK_TYPED";
/// The daemon's socket, handed by a `check` to the scan it starts.
pub const SOCKET_ENV: &str = "CARRICK_TYPED_SOCKET";
/// Which services that scan answers through the daemon: `*` for every one, or
/// a JSON array of the service roots the daemon holds a built program for.
pub const ROUTE_ENV: &str = "CARRICK_TYPED_ROUTE";
/// A file the scan appends each service root it answered without the daemon
/// to, so the `check` can warm the daemon for it afterwards.
pub const MISSES_ENV: &str = "CARRICK_TYPED_MISSES";

/// The sidecar's setting for the heap its held services are kept under.
pub const HEAP_CAP_ENV: &str = "CARRICK_TYPED_HEAP_CAP_MB";
/// The most the daemon holds by default, whatever the machine (L2).
const HEAP_CAP_CEILING_MB: u64 = 3072;
/// The default share of physical memory it holds: an eighth, so 2048 MB on a
/// 16 GB laptop and the ceiling from 24 GB (L2).
const HEAP_CAP_RAM_DIVISOR: u64 = 8;

/// How long `stop` waits for the daemon to exit after asking it, and again
/// after SIGTERM, before the next step. A daemon answering a request reads
/// neither until the request ends (one event loop).
const STOP_GRACE: Duration = Duration::from_secs(5);
/// How long a check that found another build waits for it to stop on its
/// token before killing it (L5, upgrade).
const RETIRE_GRACE: Duration = Duration::from_secs(2);

/// Each crash of a daemon, one line `<unix secs> <build id>`, inside `.carrick/`.
const CRASHES: &str = "typed.crashes";
/// Crashes of one build within this window back it off (L2).
const CRASH_WINDOW_SECS: u64 = 60 * 60;
/// How long a backed-off build is not started again.
const BACK_OFF_SECS: u64 = 24 * 60 * 60;
/// How far `ps`'s start time may sit from the record's `started_at` and still
/// be the same process (L5 (c)).
const START_TIME_SLACK_SECS: i64 = 2;

/// Whether this process may use the daemon at all.
pub fn enabled() -> bool {
    if cfg!(not(unix)) {
        return false;
    }
    !matches!(
        std::env::var(OFF_ENV).ok().as_deref().map(str::trim),
        Some("off" | "0" | "false")
    )
}

/// The build id of the sidecar whose entry script is `entry`
/// (`<dir>/dist/src/index.js`): sha256 over every file beneath the entry's
/// directory, by `/`-separated relative path in sorted order, each line
/// `<path>\0<sha256 of its content>\n`, then one line `pkg:<name>\0<version>\n`
/// per [`COMPILER_PACKAGES`] entry as installed for the sidecar (empty when
/// not found). The same hash as the sidecar's
/// `buildIdOf` (`src/sidecar/src/typed-server.ts`), which the daemon compares
/// every `hello` against.
pub fn build_id(entry: &Path) -> std::io::Result<String> {
    let root = entry.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the sidecar entry has no directory",
        )
    })?;
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut hash = Sha256::new();
    for relative in files {
        let content = std::fs::read(root.join(&relative))?;
        let digest = hex(&Sha256::digest(&content));
        hash.update(format!("{relative}\0{digest}\n").as_bytes());
    }
    // The compiler the sidecar runs is part of its build: the same sources
    // over another TypeScript answer differently.
    let sidecar_root = root.parent().and_then(Path::parent);
    for package in COMPILER_PACKAGES {
        let version = sidecar_root
            .and_then(|start| installed_version(start, package))
            .unwrap_or_default();
        hash.update(format!("pkg:{package}\0{version}\n").as_bytes());
    }
    Ok(hex(&hash.finalize()))
}

/// The packages whose installed versions are part of the build id, in the
/// order they are hashed (the sidecar's `buildIdOf` takes the same):
/// `@ts-morph/common` carries the compiler ts-morph runs.
const COMPILER_PACKAGES: [&str; 3] = ["typescript", "ts-morph", "@ts-morph/common"];

/// The `version` of `node_modules/<package>/package.json` in the first of
/// `start` and its ancestors that has one, as node resolves it from there.
/// `None` when none has one, or the first found does not parse or carries no
/// string version.
fn installed_version(start: &Path, package: &str) -> Option<String> {
    let manifest = start
        .ancestors()
        .map(|dir| dir.join("node_modules").join(package).join("package.json"))
        .find(|manifest| manifest.is_file())?;
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("version")?.as_str().map(str::to_string)
}

/// Every regular file under `dir`, relative to `root` with `/` separators.
/// Entries that are neither a directory nor a file (a symlink) are skipped,
/// as the sidecar's walk skips them.
fn collect_files(root: &Path, dir: &Path, files: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let parts: Vec<String> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            files.push(parts.join("/"));
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// What `<workspace>/.carrick/typed.json` holds, as far as this side reads it.
/// The fields beyond `pid` and `socket` prove whose record it is (L5); a
/// record without them proves nothing, so nothing is signalled on its word.
#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    pub pid: u32,
    pub socket: PathBuf,
    #[serde(default)]
    pub build_id: Option<String>,
    /// The workspace's real path, as the daemon read it.
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    /// The secret a `stop` carries to be obeyed from any build.
    #[serde(default)]
    pub token: Option<String>,
    /// When the daemon started (RFC 3339).
    #[serde(default)]
    pub started_at: Option<String>,
}

impl Record {
    fn started_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let text = self.started_at.as_deref()?;
        chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|at| at.with_timezone(&chrono::Utc))
    }
}

/// The daemon's record for `workspace`.
pub fn record_path(workspace: &Path) -> PathBuf {
    workspace.join(".carrick").join("typed.json")
}

/// The record for `workspace`, or `None` when there is none or it does not
/// parse.
pub fn read_record(workspace: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(record_path(workspace)).ok()?;
    serde_json::from_str(&text).ok()
}

/// What the daemon's `status` says of one service it holds.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeldService {
    pub repo_root: PathBuf,
    /// Whether it holds a built program for it now. A service it released to
    /// keep under its heap cap is held but not built.
    #[serde(default)]
    pub built: bool,
    /// `built`, `released`, `cold_only:too_big` or `cold_only:no_room` (L2).
    #[serde(default)]
    pub state: Option<String>,
    /// The heap its program took when it was built.
    #[serde(default)]
    pub footprint_mb: Option<f64>,
    #[serde(default)]
    pub items: HeldItems,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl HeldService {
    /// Whether it holds a built program for the service now.
    pub fn is_built(&self) -> bool {
        match self.state.as_deref() {
            Some(state) => state == "built",
            None => self.built,
        }
    }

    /// Whether it answers this service only cold, for the rest of its life:
    /// a miss on it warms nothing (L1).
    pub fn is_cold_only(&self) -> bool {
        self.state
            .as_deref()
            .is_some_and(|state| state.starts_with("cold_only"))
    }
}

/// The `infer` items the daemon keeps for a service, and how often it has
/// answered one again (`replayed`) or read it afresh (`recomputed`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HeldItems {
    #[serde(default)]
    pub held: u64,
    #[serde(default)]
    pub replayed: u64,
    #[serde(default)]
    pub recomputed: u64,
}

/// One daemon-only event in the daemon's `status` (L6): counts, MB and a
/// service's path, never anything the scanned code said.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DaemonEvent {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub detail: serde_json::Value,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

/// What the daemon's `status` says of itself. Fields this side does not read
/// are kept, so `carrick status --json` passes the whole object on.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Status {
    pub pid: u32,
    pub socket: PathBuf,
    pub services: Vec<HeldService>,
    #[serde(default)]
    pub rss_mb: f64,
    #[serde(default)]
    pub heap_cap_mb: Option<f64>,
    /// A warm-up's connection is open: a check treats the daemon as busy.
    #[serde(default)]
    pub warming: bool,
    #[serde(default)]
    pub errors: Vec<DaemonEvent>,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl Status {
    /// The roots of the services it holds a built program for.
    pub fn built(&self) -> BTreeSet<PathBuf> {
        self.services
            .iter()
            .filter(|service| service.is_built())
            .map(|service| service.repo_root.clone())
            .collect()
    }

    /// The roots of the services it answers only cold.
    pub fn cold_only(&self) -> BTreeSet<PathBuf> {
        self.services
            .iter()
            .filter(|service| service.is_cold_only())
            .map(|service| service.repo_root.clone())
            .collect()
    }
}

/// Which kind of connection a `hello` opens (L3): a check's (`probe`), which
/// is answered only for services the daemon already holds built, or the
/// warm-up's (`warm`), the only kind that builds programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloMode {
    Probe,
    Warm,
}

impl HelloMode {
    pub fn wire(self) -> &'static str {
        match self {
            HelloMode::Probe => "probe",
            HelloMode::Warm => "warm",
        }
    }
}

/// What [`probe`] found for a workspace.
#[derive(Debug, Clone)]
pub enum Probe {
    /// No daemon: no record, a dead process, or a socket nobody accepts on.
    Absent,
    /// A daemon accepted the connection and did not answer in time: it is
    /// answering a request for someone else. Its build is unknown.
    Busy { pid: u32 },
    /// A daemon of another build refused this one's `hello`.
    Mismatch { pid: u32 },
    /// A daemon of this build, and what it holds.
    Serving(Status),
}

/// Whether a daemon serves `workspace` on the build `build_id`, waiting at
/// most `wait` for each answer: `hello`, then `status`.
///
/// A daemon is used only on proof that it is this workspace's (L5 (b)): its
/// socket is owned by this user, and its `hello` answer names the record's
/// pid and this workspace's real path. A record copied with a workspace names
/// another workspace's daemon and is not used. A record whose process is gone
/// is a crash: it is noted ([`backed_off`]) and the record removed.
pub fn probe(workspace: &Path, build_id: &str, wait: Duration) -> Probe {
    let Some(record) = read_record(workspace) else {
        return Probe::Absent;
    };
    if !pid_alive(record.pid) {
        note_crash(workspace, record.build_id.as_deref().unwrap_or(""));
        remove_record_of(workspace, &record);
        return Probe::Absent;
    }
    if !socket_is_ours(&record.socket) {
        return Probe::Absent;
    }
    probe_socket(&record, workspace, build_id, wait)
}

/// Whether `socket` is owned by the user this process runs as.
fn socket_is_ours(socket: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        std::fs::metadata(socket).is_ok_and(|meta| meta.uid() == uid)
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        false
    }
}

/// Whether a `hello` answer comes from the daemon `record` names, serving
/// `workspace`: it carries that pid and the workspace's real path.
fn answered_as(frame: &serde_json::Value, record: &Record, workspace: &Path) -> bool {
    let pid = frame.get("pid").and_then(|p| p.as_u64());
    let named = frame
        .get("workspace")
        .and_then(|w| w.as_str())
        .map(PathBuf::from);
    let real = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    pid == Some(u64::from(record.pid))
        && named.is_some_and(|named| named == real || named == workspace)
}

/// A `hello` naming this side's build and connection kind.
fn hello(build_id: &str, mode: HelloMode) -> serde_json::Value {
    serde_json::json!({ "action": "hello", "build_id": build_id, "mode": mode.wire() })
}

#[cfg(unix)]
fn probe_socket(record: &Record, workspace: &Path, build_id: &str, wait: Duration) -> Probe {
    let Ok(mut conn) = Conn::open(&record.socket, wait) else {
        return Probe::Absent;
    };
    match conn.ask(hello(build_id, HelloMode::Probe)) {
        Answer::Frame(frame) if !answered_as(&frame, record, workspace) => {
            return Probe::Absent;
        }
        Answer::Frame(frame) if succeeded(&frame) => {}
        Answer::Frame(frame) if mismatched(&frame) => {
            return Probe::Mismatch { pid: record.pid };
        }
        Answer::Frame(_) | Answer::Closed => return Probe::Absent,
        Answer::TimedOut => return Probe::Busy { pid: record.pid },
    }
    match conn.ask(serde_json::json!({ "action": "status" })) {
        Answer::Frame(frame) => match frame
            .get("daemon")
            .cloned()
            .map(serde_json::from_value::<Status>)
        {
            Some(Ok(status)) => Probe::Serving(status),
            _ => Probe::Absent,
        },
        Answer::TimedOut => Probe::Busy { pid: record.pid },
        Answer::Closed => Probe::Absent,
    }
}

#[cfg(not(unix))]
fn probe_socket(_record: &Record, _workspace: &Path, _build_id: &str, _wait: Duration) -> Probe {
    Probe::Absent
}

fn succeeded(frame: &serde_json::Value) -> bool {
    frame.get("status").and_then(|s| s.as_str()) == Some("success")
}

fn mismatched(frame: &serde_json::Value) -> bool {
    frame
        .get("errors")
        .and_then(|errors| errors.as_array())
        .is_some_and(|errors| {
            errors
                .iter()
                .any(|e| e.as_str().is_some_and(|e| e.starts_with("build_mismatch")))
        })
}

/// One answer read off the socket.
enum Answer {
    Frame(serde_json::Value),
    TimedOut,
    Closed,
}

/// A short synchronous conversation with the daemon: one request, one
/// terminal answer, progress frames skipped.
#[cfg(unix)]
struct Conn {
    stream: std::os::unix::net::UnixStream,
    reader: BufReader<std::os::unix::net::UnixStream>,
    next: u64,
}

#[cfg(unix)]
impl Conn {
    fn open(socket: &Path, wait: Duration) -> std::io::Result<Self> {
        let stream = std::os::unix::net::UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(wait))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            stream,
            reader,
            next: 0,
        })
    }

    fn ask(&mut self, mut request: serde_json::Value) -> Answer {
        self.next += 1;
        let id = format!("cli-{}", self.next);
        request["request_id"] = serde_json::Value::String(id.clone());
        if writeln!(self.stream, "{request}")
            .and_then(|()| self.stream.flush())
            .is_err()
        {
            return Answer::Closed;
        }
        let mut line = String::new();
        loop {
            line.clear();
            match self.reader.read_line(&mut line) {
                Ok(0) => return Answer::Closed,
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Answer::TimedOut;
                }
                Err(_) => return Answer::Closed,
            }
            let Ok(frame) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if frame.get("status").and_then(|s| s.as_str()) == Some("progress") {
                continue;
            }
            // A refused `hello` may answer under the id it was sent with or
            // close at once; either way this is the answer.
            if frame.get("request_id").and_then(|r| r.as_str()) == Some(id.as_str())
                || mismatched(&frame)
            {
                return Answer::Frame(frame);
            }
        }
    }
}

/// Whether process `pid` exists.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 {
            return false;
        }
        // SAFETY: signal 0 sends nothing; it only asks whether `pid` exists.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Whether the process `record` names is the daemon that wrote it, for
/// `workspace`, when its socket cannot say (L5 (c)): `ps` shows it running
/// `--serve <workspace>`, and the start time `ps` gives sits within
/// [`START_TIME_SLACK_SECS`] of the record's `started_at`. A record outlives
/// its daemon when the daemon is killed, and the kernel hands its pid to the
/// next process, so a pid read from a record is never signalled on the
/// record's word alone.
pub fn identity(record: &Record, workspace: &Path) -> bool {
    let pid = record.pid;
    if pid == 0 || !pid_alive(pid) {
        return false;
    }
    let Some(started_at) = record.started_at() else {
        return false;
    };
    // `-ww`: the whole command line, however long, with no terminal to fit.
    let Ok(out) = Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-ww", "-o", "etime=,command=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some((etime, command)) = text.trim().split_once(char::is_whitespace) else {
        return false;
    };
    let Some(elapsed) = parse_etime(etime) else {
        return false;
    };
    let started = chrono::Utc::now() - chrono::Duration::seconds(elapsed as i64);
    if (started - started_at).num_seconds().abs() > START_TIME_SLACK_SECS {
        return false;
    }
    let serves = |path: &Path| command.contains(&format!(" --serve {}", path.display()));
    serves(workspace)
        || record.workspace.as_deref().is_some_and(serves)
        || workspace.canonicalize().is_ok_and(|real| serves(&real))
}

/// `ps`'s elapsed time, `[[dd-]hh:]mm:ss`, in seconds.
fn parse_etime(etime: &str) -> Option<u64> {
    let (days, clock) = match etime.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, etime),
    };
    let mut seconds = 0u64;
    for part in clock.split(':') {
        seconds = seconds * 60 + part.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + seconds)
}

/// Send `signal` to `pid` and wait up to `grace` for it to be gone; whether
/// it is. Callers prove the pid first ([`identity`], or a `hello` answer
/// naming it).
fn signal_and_wait(pid: u32, signal: i32, grace: Duration) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: a signal to a pid whose identity the caller proved.
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
    #[cfg(not(unix))]
    let _ = signal;
    wait_gone(pid, grace)
}

/// Wait up to `grace` for `pid` to be gone; whether it is.
fn wait_gone(pid: u32, grace: Duration) -> bool {
    let waited = Instant::now();
    while pid_alive(pid) && waited.elapsed() < grace {
        std::thread::sleep(Duration::from_millis(20));
    }
    !pid_alive(pid)
}

/// The heap a new daemon keeps its held services under, unless
/// [`HEAP_CAP_ENV`] says otherwise: an eighth of physical memory, at most
/// [`HEAP_CAP_CEILING_MB`] (L2).
pub fn default_heap_cap_mb(total_memory_mb: Option<u64>) -> u64 {
    total_memory_mb.map_or(HEAP_CAP_CEILING_MB, |total| {
        (total / HEAP_CAP_RAM_DIVISOR).min(HEAP_CAP_CEILING_MB)
    })
}

/// Start a daemon for `workspace` from the sidecar whose entry script is
/// `entry`, detached: in a process group of its own, with no terminal and no
/// pipe back to this process, so it outlives the command that started it. Its
/// log is `<workspace>/.carrick/typed.log`. Returns once the process exists;
/// [`wait_serving`] waits for it to answer.
///
/// A daemon already serving the workspace is not replaced: the new process
/// finds its record and exits (carrick#2879).
pub fn start_detached(workspace: &Path, entry: &Path) -> std::io::Result<()> {
    let carrick = workspace.join(".carrick");
    std::fs::create_dir_all(&carrick)?;
    let log = std::fs::File::create(carrick.join("typed.log"))?;
    let mut command = Command::new("node");
    // V8 flags precede the script, or node hands them to it. The collector
    // is exposed so the daemon can release a service and get its heap back
    // at once (carrick#2881). V8's limit is the cold path's own, so a request
    // a process of its own answers does not abort the daemon (L2).
    command.arg("--expose-gc");
    if let Some(mb) = crate::services::type_sidecar::heap_cap_mb() {
        command.arg(format!("--max-old-space-size={mb}"));
    }
    command
        .arg(entry)
        .arg("--serve")
        .arg(workspace)
        .arg("--cli-version")
        .arg(env!("CARGO_PKG_VERSION"));
    if std::env::var_os(HEAP_CAP_ENV).is_none() {
        let cap = default_heap_cap_mb(crate::services::type_sidecar::total_memory_mb());
        command.env(HEAP_CAP_ENV, cap.to_string());
    }
    // The answers a scan's own processes keep, it keeps too (carrick#2843):
    // the same directory a local scan hands its sidecars.
    if std::env::var_os(crate::services::type_sidecar::INFER_CACHE_ENV).is_none()
        && let Some(dir) = crate::services::type_sidecar::local_infer_cache_dir()
    {
        command.env(crate::services::type_sidecar::INFER_CACHE_ENV, dir);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()?;
    Ok(())
}

/// Wait up to `timeout` for a daemon of build `build_id` to serve
/// `workspace`.
pub fn wait_serving(workspace: &Path, build_id: &str, timeout: Duration) -> Option<Status> {
    let started = Instant::now();
    loop {
        if let Probe::Serving(status) = probe(workspace, build_id, Duration::from_secs(1)) {
            return Some(status);
        }
        if started.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Stop the daemon serving `workspace`, if one is (L5): `stop` carrying the
/// record's token, which it obeys from any build; [`STOP_GRACE`] for it to
/// exit; then, only when [`identity`] proves the pid is that daemon, SIGTERM,
/// [`STOP_GRACE`] again, and SIGKILL. A pid it cannot prove is another
/// process: it gets no signal, and the stale record is removed. Returns one
/// line saying what happened.
pub fn stop(workspace: &Path) -> String {
    let Some(record) = read_record(workspace) else {
        return "No type daemon is running for this workspace.".to_string();
    };
    let pid = record.pid;
    if !pid_alive(pid) {
        remove_record_of(workspace, &record);
        return format!(
            "No type daemon is running for this workspace (removed the record of pid {pid}, which had exited)."
        );
    }
    if ask_to_stop(&record) && wait_gone(pid, STOP_GRACE) {
        return format!("Stopped the type daemon (pid {pid}).");
    }
    if !identity(&record, workspace) {
        remove_record_of(workspace, &record);
        return format!(
            "No type daemon is running for this workspace (removed a stale record: pid {pid} is not its daemon and was left running)."
        );
    }
    let ended = if signal_and_wait(pid, libc::SIGTERM, STOP_GRACE) {
        "SIGTERM"
    } else {
        signal_and_wait(pid, libc::SIGKILL, STOP_GRACE);
        "SIGKILL"
    };
    remove_record_of(workspace, &record);
    format!("Stopped the type daemon (pid {pid}) with {ended}: it did not stop when asked.")
}

/// Ask the daemon `record` names to stop, with the record's token as the
/// connection's first frame: obeyed from any build (L5).
#[cfg(unix)]
fn ask_to_stop(record: &Record) -> bool {
    let Some(token) = record.token.as_deref() else {
        return false;
    };
    let Ok(mut conn) = Conn::open(&record.socket, Duration::from_secs(2)) else {
        return false;
    };
    matches!(
        conn.ask(serde_json::json!({ "action": "stop", "token": token })),
        Answer::Frame(frame) if succeeded(&frame)
    )
}

#[cfg(not(unix))]
fn ask_to_stop(_record: &Record) -> bool {
    false
}

/// Clear the way for a new daemon of `workspace` when the one its record names,
/// `pid`, runs another build (L5, upgrade): asked to stop on its token, given
/// [`RETIRE_GRACE`], then killed only on proven [`identity`]. Either way its
/// record is removed, so the new daemon does not find a live process named in
/// it and exit, and an old daemon left running stops itself once its record is
/// gone.
pub fn retire(workspace: &Path, pid: u32) {
    let Some(record) = read_record(workspace).filter(|record| record.pid == pid) else {
        return;
    };
    let stopped = ask_to_stop(&record) && wait_gone(pid, RETIRE_GRACE);
    if !stopped && identity(&record, workspace) {
        signal_and_wait(pid, libc::SIGKILL, RETIRE_GRACE);
    }
    remove_record_of(workspace, &record);
}

/// Note a crash of the daemon of build `build_id`: its record named a process
/// that is gone. A daemon that stops on its own removes its record first, so a
/// record left behind is a crash (an out-of-heap abort, a kill).
pub fn note_crash(workspace: &Path, build_id: &str) {
    let path = workspace.join(".carrick").join(CRASHES);
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{secs} {build_id}");
    }
}

/// A build that is not started again for now, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackOff {
    /// Unix seconds until which no daemon of the build is started.
    pub until: u64,
    /// Crashes of the build on record.
    pub crashes: usize,
}

/// Whether the daemon of build `build_id` is backed off in `workspace`: two
/// of its crashes fell within [`CRASH_WINDOW_SECS`] of each other, and the
/// latest is less than [`BACK_OFF_SECS`] ago (L2). A new build starts afresh.
pub fn backed_off(workspace: &Path, build_id: &str) -> Option<BackOff> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    back_off_at(
        &std::fs::read_to_string(workspace.join(".carrick").join(CRASHES)).ok()?,
        build_id,
        now,
    )
}

fn back_off_at(crashes: &str, build_id: &str, now: u64) -> Option<BackOff> {
    let mut times: Vec<u64> = crashes
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(_, build)| build.trim() == build_id)
        .filter_map(|(secs, _)| secs.parse().ok())
        .collect();
    times.sort_unstable();
    let latest = *times.last()?;
    let close = times
        .windows(2)
        .any(|pair| pair[1] - pair[0] <= CRASH_WINDOW_SECS);
    let until = latest + BACK_OFF_SECS;
    (close && now < until).then_some(BackOff {
        until,
        crashes: times.len(),
    })
}

/// Remove the record and its socket while the record still names `record`'s
/// process: a daemon that was killed cannot remove its own.
fn remove_record_of(workspace: &Path, record: &Record) {
    if read_record(workspace).is_some_and(|now| now.pid == record.pid) {
        let _ = std::fs::remove_file(&record.socket);
        let _ = std::fs::remove_file(record_path(workspace));
    }
}

/// What `carrick status` says about the daemon serving `workspace` (L6): at
/// most four lines, none when there is no record of one and no back-off, and
/// the daemon's whole `status` object for `--json` when it answered.
pub fn status_report(
    workspace: &Path,
    build_id: Option<&str>,
) -> (Vec<String>, Option<serde_json::Value>) {
    let mut lines = Vec::new();
    let mut object = None;
    let back_off = build_id.and_then(|id| backed_off(workspace, id));
    if let Some(record) = read_record(workspace) {
        let pid = record.pid;
        match build_id.map(|id| probe(workspace, id, Duration::from_secs(1))) {
            Some(Probe::Serving(status)) => {
                lines.extend(serving_lines(&record, &status, workspace));
                object = serde_json::to_value(&status).ok();
            }
            Some(Probe::Busy { .. }) => lines.push(format!(
                "Type daemon: pid {pid}, answering a request; status after it."
            )),
            Some(Probe::Mismatch { .. }) => lines.push(format!(
                "Type daemon: pid {pid} runs another build of the sidecar; the next check replaces it."
            )),
            Some(Probe::Absent) | None if !pid_alive(pid) => lines.push(format!(
                "Type daemon: not running (pid {pid} in its record has exited)."
            )),
            Some(Probe::Absent) | None => lines.push(format!(
                "Type daemon: pid {pid}, not answering as this workspace's daemon."
            )),
        }
    }
    if let Some(back_off) = back_off {
        let until = chrono::DateTime::<chrono::Utc>::from_timestamp(back_off.until as i64, 0)
            .map(|at| at.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default();
        lines.truncate(3);
        lines.push(format!(
            "Type daemon: off until {until} after {} crashes; log .carrick/typed.log",
            back_off.crashes
        ));
    }
    (lines, object)
}

/// The lines for a daemon that answered: itself; its largest held services;
/// its last daemon-only events.
fn serving_lines(record: &Record, status: &Status, workspace: &Path) -> Vec<String> {
    let uptime = record
        .started_at()
        .map(|at| (chrono::Utc::now() - at).num_minutes())
        .map(|minutes| format!(", up {minutes} min"))
        .unwrap_or_default();
    let cap = status
        .heap_cap_mb
        .map(|cap| format!(" (cap {cap:.0} MB)"))
        .unwrap_or_default();
    let warming = if status.warming { ", warming" } else { "" };
    let mut lines = vec![format!(
        "Type daemon: pid {}{uptime}, {:.0} MB resident{cap}, {} service(s) held, {} built{warming}.",
        status.pid,
        status.rss_mb,
        status.services.len(),
        status.built().len(),
    )];
    let relative = |path: &Path| -> String {
        path.strip_prefix(workspace)
            .ok()
            .filter(|rel| !rel.as_os_str().is_empty())
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let mut held: Vec<&HeldService> = status
        .services
        .iter()
        .filter(|service| service.footprint_mb.is_some())
        .collect();
    held.sort_by(|a, b| {
        b.footprint_mb
            .unwrap_or(0.0)
            .total_cmp(&a.footprint_mb.unwrap_or(0.0))
    });
    if !held.is_empty() {
        let largest: Vec<String> = held
            .iter()
            .take(5)
            .map(|service| {
                format!(
                    "{} {:.0} MB",
                    relative(&service.repo_root),
                    service.footprint_mb.unwrap_or(0.0)
                )
            })
            .collect();
        lines.push(format!("  Largest held: {}.", largest.join(", ")));
    }
    if !status.errors.is_empty() {
        let recent: Vec<String> = status
            .errors
            .iter()
            .rev()
            .take(3)
            .map(|event| match &event.service {
                Some(service) => format!("{} ({service})", event.kind),
                None => event.kind.clone(),
            })
            .collect();
        lines.push(format!("  Recent daemon events: {}.", recent.join(", ")));
    }
    lines
}

/// Which services a scan answers through the daemon.
#[derive(Debug, Clone)]
pub enum Route {
    /// Every service: the warm-up run, whose purpose is to fill the daemon.
    All,
    /// The services whose root the daemon holds a built program for. Any
    /// other is answered by a process of its own, at today's width, rather
    /// than queued behind the rest in the daemon's one queue.
    Held(BTreeSet<PathBuf>),
}

/// The daemon a scan answers through, as the `check` that started it handed
/// it over in [`SOCKET_ENV`], [`ROUTE_ENV`] and [`MISSES_ENV`].
#[derive(Debug, Clone)]
pub struct Use {
    socket: PathBuf,
    route: Route,
    misses: Option<PathBuf>,
}

impl Use {
    pub fn new(socket: PathBuf, route: Route, misses: Option<PathBuf>) -> Self {
        Self {
            socket,
            route,
            misses,
        }
    }

    /// The daemon this process was handed, or `None` when it was handed none
    /// or the daemon is off ([`OFF_ENV`]).
    pub fn from_env() -> Option<Self> {
        if !enabled() {
            return None;
        }
        let socket = PathBuf::from(std::env::var_os(SOCKET_ENV)?);
        let route = match std::env::var(ROUTE_ENV).ok()?.trim() {
            "*" => Route::All,
            listed => Route::Held(serde_json::from_str::<BTreeSet<PathBuf>>(listed).ok()?),
        };
        let misses = std::env::var_os(MISSES_ENV).map(PathBuf::from);
        Some(Self::new(socket, route, misses))
    }

    /// Hand this daemon to the scan `command` starts.
    pub fn apply(&self, command: &mut Command) {
        command.env(SOCKET_ENV, &self.socket);
        let route = match &self.route {
            Route::All => "*".to_string(),
            Route::Held(roots) => serde_json::to_string(roots).unwrap_or_else(|_| "[]".into()),
        };
        command.env(ROUTE_ENV, route);
        match &self.misses {
            Some(misses) => command.env(MISSES_ENV, misses),
            None => command.env_remove(MISSES_ENV),
        };
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The kind of connection its services open: the warm-up's routes every
    /// service and builds; a check's routes only what the daemon holds built.
    pub fn mode(&self) -> HelloMode {
        match self.route {
            Route::All => HelloMode::Warm,
            Route::Held(_) => HelloMode::Probe,
        }
    }

    /// Whether the service at `repo_root` answers through the daemon.
    pub fn routes(&self, repo_root: &Path) -> bool {
        match &self.route {
            Route::All => true,
            Route::Held(roots) => roots.contains(repo_root),
        }
    }

    /// Record that the service at `repo_root` was answered without the
    /// daemon, for the `check` to warm it afterwards.
    pub fn note_miss(&self, repo_root: &Path) {
        let Some(misses) = &self.misses else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(misses)
        {
            let _ = writeln!(file, "{}", repo_root.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id is the sidecar's own: the same files hashed the same way give
    /// the same id, and any byte of any file moves it.
    #[test]
    fn a_build_id_moves_with_any_file_and_only_with_one() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("dist/src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("index.js"), "one").unwrap();
        std::fs::write(src.join("nested/a.js"), "two").unwrap();
        let entry = src.join("index.js");
        let first = build_id(&entry).unwrap();
        assert_eq!(first, build_id(&entry).unwrap());
        std::fs::write(src.join("nested/a.js"), "three").unwrap();
        assert_ne!(first, build_id(&entry).unwrap());
    }

    /// The installed compiler is part of the build: a TypeScript reinstall at
    /// another version moves the id, found by walking up from the sidecar.
    #[test]
    fn a_build_id_moves_with_the_installed_typescript() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("sidecar/dist/src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("index.js"), "one").unwrap();
        let entry = src.join("index.js");
        let none = build_id(&entry).unwrap();
        // Hoisted: one level above the sidecar root.
        let ts = dir.path().join("node_modules/typescript");
        std::fs::create_dir_all(&ts).unwrap();
        std::fs::write(ts.join("package.json"), r#"{"version":"5.9.3"}"#).unwrap();
        let first = build_id(&entry).unwrap();
        assert_ne!(none, first);
        std::fs::write(ts.join("package.json"), r#"{"version":"6.0.2"}"#).unwrap();
        assert_ne!(first, build_id(&entry).unwrap());
    }

    /// Two crashes of one build within the hour back it off for a day from
    /// the latest; crashes far apart, or of another build, do not.
    #[test]
    fn two_close_crashes_back_a_build_off_for_a_day() {
        let crashes = "1000 b\n1000 other\n4000 b\n";
        let off = back_off_at(crashes, "b", 4001).unwrap();
        assert_eq!(off.until, 4000 + BACK_OFF_SECS);
        assert_eq!(off.crashes, 2);
        assert!(back_off_at(crashes, "b", 4000 + BACK_OFF_SECS).is_none());
        assert!(back_off_at(crashes, "other", 4001).is_none());
        assert!(back_off_at("1000 b\n9000 b\n", "b", 9001).is_none());
    }

    #[test]
    fn ps_elapsed_times_read_as_seconds() {
        assert_eq!(parse_etime("05"), Some(5));
        assert_eq!(parse_etime("01:05"), Some(65));
        assert_eq!(parse_etime("02:01:05"), Some(7265));
        assert_eq!(parse_etime("3-02:01:05"), Some(3 * 86_400 + 7265));
        assert_eq!(parse_etime("x"), None);
    }

    /// An eighth of the machine, at most 3 GB.
    #[test]
    fn the_default_heap_cap_is_an_eighth_of_memory_at_most_three_gb() {
        assert_eq!(default_heap_cap_mb(Some(16 * 1024)), 2048);
        assert_eq!(default_heap_cap_mb(Some(64 * 1024)), 3072);
        assert_eq!(default_heap_cap_mb(None), 3072);
    }

    /// A live process that is not a `--serve` daemon is never proven one,
    /// whatever its record says.
    #[test]
    fn a_process_that_is_not_a_daemon_has_no_identity() {
        let record = Record {
            pid: std::process::id(),
            socket: PathBuf::from("/nonexistent"),
            build_id: None,
            workspace: Some(PathBuf::from("/w")),
            token: None,
            started_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        assert!(!identity(&record, Path::new("/w")));
    }

    /// A route lists the roots the daemon holds a built program for, and a
    /// scan reads back exactly what the `check` handed it.
    #[test]
    fn a_route_reaches_the_scan_unchanged() {
        let held: BTreeSet<PathBuf> = [PathBuf::from("/w/a"), PathBuf::from("/w/b c")].into();
        let daemon = Use::new(
            PathBuf::from("/w/.carrick/typed.sock"),
            Route::Held(held),
            None,
        );
        let mut command = Command::new("true");
        daemon.apply(&mut command);
        let route = command
            .get_envs()
            .find(|(key, _)| *key == ROUTE_ENV)
            .and_then(|(_, value)| value)
            .and_then(|value| value.to_str())
            .unwrap()
            .to_string();
        let read: BTreeSet<PathBuf> = serde_json::from_str(&route).unwrap();
        assert!(read.contains(Path::new("/w/b c")));
        assert!(daemon.routes(Path::new("/w/a")));
        assert!(!daemon.routes(Path::new("/w/d")));
    }
}
