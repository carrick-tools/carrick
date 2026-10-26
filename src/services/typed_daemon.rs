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
//! - [`start_detached`], [`kill`] and [`stop`]: its lifecycle. It is started
//!   by the CLI, lazily, as a process of the user's own, never as a system
//!   service;
//! - [`Use`]: what a `check` hands the scan it starts, so the scan's services
//!   connect to the daemon instead of each spawning a process
//!   ([`crate::services::type_sidecar::SidecarRecipe::with_daemon`]).
//!
//! When to start, use or warm it is `local_mode::typed`'s.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
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

/// The heap the daemon keeps its held services under (the sidecar's
/// `CARRICK_TYPED_HEAP_CAP_MB`): the re-scan's per-process cap, so the daemon
/// holds about what one re-scanned service's process held.
const HEAP_CAP_MB: u64 = 3072;
/// V8's own limit for the daemon, above [`HEAP_CAP_MB`]: the cap is where the
/// daemon starts releasing services, and the request that crossed it still
/// has to finish.
const OLD_SPACE_MB: u64 = 4096;

/// How long `stop` waits for the daemon to exit before killing it. `stop`
/// waits for the request it is answering, which on a large service is minutes
/// (carrick#2884, the server-mode note).
const STOP_GRACE: Duration = Duration::from_secs(5);

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
#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    pub pid: u32,
    pub socket: PathBuf,
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
#[derive(Debug, Clone, Deserialize)]
pub struct HeldService {
    pub repo_root: PathBuf,
    /// Whether it holds a built program for it now. A service it released to
    /// keep under its heap cap is held but not built.
    pub built: bool,
    #[serde(default)]
    pub items: HeldItems,
}

/// The `infer` items the daemon keeps for a service, and how often it has
/// answered one again (`replayed`) or read it afresh (`recomputed`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HeldItems {
    #[serde(default)]
    pub held: u64,
    #[serde(default)]
    pub replayed: u64,
    #[serde(default)]
    pub recomputed: u64,
}

/// What the daemon's `status` says of itself.
#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub socket: PathBuf,
    pub services: Vec<HeldService>,
    #[serde(default)]
    pub rss_mb: u64,
}

impl Status {
    /// The roots of the services it holds a built program for.
    pub fn built(&self) -> BTreeSet<PathBuf> {
        self.services
            .iter()
            .filter(|service| service.built)
            .map(|service| service.repo_root.clone())
            .collect()
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
pub fn probe(workspace: &Path, build_id: &str, wait: Duration) -> Probe {
    let Some(record) = read_record(workspace) else {
        return Probe::Absent;
    };
    if !pid_alive(record.pid) {
        return Probe::Absent;
    }
    probe_socket(&record, build_id, wait)
}

#[cfg(unix)]
fn probe_socket(record: &Record, build_id: &str, wait: Duration) -> Probe {
    let Ok(mut conn) = Conn::open(&record.socket, wait) else {
        return Probe::Absent;
    };
    let hello = serde_json::json!({ "action": "hello", "build_id": build_id });
    match conn.ask(hello) {
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
fn probe_socket(_record: &Record, _build_id: &str, _wait: Duration) -> Probe {
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

/// Whether process `pid` is the type daemon of `workspace`: its command line
/// runs the sidecar in server mode (`--serve`) for this workspace. A record
/// outlives its daemon when the daemon is killed, and the kernel hands its pid
/// to the next process, so a pid read from a record is never signalled on the
/// record's word alone.
pub fn is_daemon_of(pid: u32, workspace: &Path) -> bool {
    if pid == 0 || !pid_alive(pid) {
        return false;
    }
    // `-ww`: the whole command line, however long, with no terminal to fit.
    let Ok(out) = Command::new("ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let command = String::from_utf8_lossy(&out.stdout);
    let names_workspace = |path: &Path| command.contains(&*path.to_string_lossy());
    command.contains(" --serve ")
        && (names_workspace(workspace)
            || workspace
                .canonicalize()
                .is_ok_and(|real| names_workspace(&real)))
}

/// Kill the type daemon of `workspace`, process `pid`, now and wait, briefly,
/// for it to be gone. Nothing is signalled unless `pid` is that daemon
/// ([`is_daemon_of`]); returns whether it was.
///
/// `stop` and SIGTERM wait for the request the daemon is answering, which on a
/// large service is minutes (carrick#2884); a daemon of another build must not
/// answer anything, so it is killed outright. The next start takes over the
/// record it leaves (carrick#2879).
pub fn kill(pid: u32, workspace: &Path) -> bool {
    if !is_daemon_of(pid, workspace) {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: SIGKILL to a process whose command line shows it is this
        // workspace's daemon.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        let waited = Instant::now();
        while pid_alive(pid) && waited.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    true
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
    // at once (carrick#2881).
    command
        .arg("--expose-gc")
        .arg(format!("--max-old-space-size={OLD_SPACE_MB}"))
        .arg(entry)
        .arg("--serve")
        .arg(workspace)
        .arg("--cli-version")
        .arg(env!("CARGO_PKG_VERSION"));
    if std::env::var_os("CARRICK_TYPED_HEAP_CAP_MB").is_none() {
        command.env("CARRICK_TYPED_HEAP_CAP_MB", HEAP_CAP_MB.to_string());
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

/// Stop the daemon serving `workspace`, if one is: `stop` when it is this
/// build and answers, killed when it is another build, does not answer, or is
/// still running [`STOP_GRACE`] after `stop`. A record left by a daemon that
/// was killed is removed. Returns one line saying what happened.
pub fn stop(workspace: &Path, build_id: Option<&str>) -> String {
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
    let asked = build_id.is_some_and(|id| ask_to_stop(&record, id));
    let waited = Instant::now();
    while asked && pid_alive(pid) && waited.elapsed() < STOP_GRACE {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !pid_alive(pid) {
        return format!("Stopped the type daemon (pid {pid}).");
    }
    if kill(pid, workspace) {
        remove_record_of(workspace, &record);
        return format!("Stopped the type daemon (pid {pid}): killed.");
    }
    // The record outlived its daemon and the pid now belongs to another
    // process, which is left alone.
    remove_record_of(workspace, &record);
    format!(
        "No type daemon is running for this workspace (removed a stale record: pid {pid} is another process, left running)."
    )
}

#[cfg(unix)]
fn ask_to_stop(record: &Record, build_id: &str) -> bool {
    let Ok(mut conn) = Conn::open(&record.socket, Duration::from_secs(2)) else {
        return false;
    };
    let hello = serde_json::json!({ "action": "hello", "build_id": build_id });
    if !matches!(conn.ask(hello), Answer::Frame(frame) if succeeded(&frame)) {
        return false;
    }
    matches!(conn.ask(serde_json::json!({ "action": "stop" })), Answer::Frame(frame) if succeeded(&frame))
}

#[cfg(not(unix))]
fn ask_to_stop(_record: &Record, _build_id: &str) -> bool {
    false
}

/// Clear the way for a new daemon of `workspace` when the one its record names,
/// `pid`, runs another build: killed when it is this workspace's daemon
/// ([`kill`]), and either way its record removed, so the new daemon does not
/// find a live process named in it and exit.
pub fn retire(workspace: &Path, pid: u32) {
    kill(pid, workspace);
    if let Some(record) = read_record(workspace).filter(|record| record.pid == pid) {
        remove_record_of(workspace, &record);
    }
}

/// Remove the record and its socket while the record still names `record`'s
/// process: a daemon that was killed cannot remove its own.
fn remove_record_of(workspace: &Path, record: &Record) {
    if read_record(workspace).is_some_and(|now| now.pid == record.pid) {
        let _ = std::fs::remove_file(&record.socket);
        let _ = std::fs::remove_file(record_path(workspace));
    }
}

/// One line for `carrick status` about the daemon serving `workspace`, or
/// `None` when there is no record of one.
pub fn status_line(workspace: &Path, build_id: Option<&str>) -> Option<String> {
    let record = read_record(workspace)?;
    let pid = record.pid;
    let line = match build_id.map(|id| probe(workspace, id, Duration::from_secs(1))) {
        Some(Probe::Serving(status)) => format!(
            "Type daemon: pid {pid}, {} service(s) held, {} with a built program, {} MB resident.",
            status.services.len(),
            status.built().len(),
            status.rss_mb
        ),
        Some(Probe::Busy { .. }) => format!("Type daemon: pid {pid}, answering a request."),
        Some(Probe::Mismatch { .. }) => format!(
            "Type daemon: pid {pid} runs another build of the sidecar; the next check replaces it."
        ),
        Some(Probe::Absent) | None if !pid_alive(pid) => {
            format!("Type daemon: not running (pid {pid} in its record has exited).")
        }
        Some(Probe::Absent) | None => {
            format!("Type daemon: pid {pid}, not answering on its socket.")
        }
    };
    Some(line)
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
