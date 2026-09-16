//! The local subcommands: parsing, dispatch, and what each one prints.
//!
//! `carrick <path>` is still the scan every CI run performs. These four names
//! are checked before that, and only as the first argument, so the only path
//! they take from the old CLI is a directory literally named `index`, `touch`,
//! `check` or `refresh`.

use std::path::{Path, PathBuf};

use super::contract::{ErrorOutput, ReadError, ReadFailure};
use super::query::{Freshness, Mode};
use super::workspace::Workspace;

/// A local command, once its arguments have been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalCommand {
    /// Read the shared workspace/service proposal without scanning or writing.
    Derive { workspace: Option<PathBuf> },
    /// Scan every repo the workspace lists, and build the index.
    ///
    /// This is the inferred scan: Carrick Cloud classifies what the
    /// deterministic passes could not, and the result is uploaded. There is no
    /// facts-only form of it — the only product is the inferred index, and a
    /// pass that states less has no user (carrick#1008). It is refused
    /// outright until every repo in the workspace has a `carrick.json`,
    /// because the one paid scan runs against a config someone has read (see
    /// [`inference_refusal`]). `refresh` still runs no model, but it is the
    /// session-start hook's command and no first-run copy names it
    /// (cloud#832).
    Index {
        workspace: Option<PathBuf>,
        /// Run the build in the background, and answer at once with its id.
        ///
        /// The ruled first run has an agent run `carrick index`, and a first
        /// paid scan of a mid-sized monorepo takes about fifteen
        /// minutes — longer than the two-minute default and the ten-minute cap
        /// of the shell an agent runs it through (carrick#992). Detached, the
        /// scan outlives that shell, its output lands in
        /// `.carrick/scan-<id>.log`, and `carrick status` says where it is.
        detach: bool,
    },
    /// What is on the other side of this file.
    Touch {
        file: PathBuf,
        workspace: Option<PathBuf>,
        json: bool,
    },
    /// The same, plus the verdicts the index already holds.
    Check {
        file: PathBuf,
        workspace: Option<PathBuf>,
        json: bool,
        /// Re-extract the file and re-judge it against the index before
        /// answering, when the tree has moved past the index (carrick#1036).
        /// The post-edit hook asks for this; nothing else does.
        recheck: bool,
    },
    /// Re-scan one service (or every repo) and re-join.
    Refresh {
        service: Option<String>,
        workspace: Option<PathBuf>,
    },
    /// What the workspace holds, with no file in the question.
    Status {
        workspace: Option<PathBuf>,
        json: bool,
    },
}

impl LocalCommand {
    /// Whether this command writes anything. The two that do are the two that
    /// log; the read-only pair stays silent so a hook's output is the answer
    /// and nothing else.
    pub fn writes(&self) -> bool {
        matches!(
            self,
            LocalCommand::Index { .. } | LocalCommand::Refresh { .. }
        )
    }
}

/// Every subcommand the binary answers. `carrick --help` names each one with a
/// description, and the test in `src/help.rs` holds it to this list.
pub const LOCAL_COMMANDS: [&str; 6] = ["derive", "index", "refresh", "status", "check", "touch"];

/// Read a local command from the argument list, or `None` when the first
/// argument is not one of those names.
pub fn parse(args: &[String]) -> Option<Result<LocalCommand, String>> {
    let name = args.first()?.as_str();
    if !LOCAL_COMMANDS.contains(&name) {
        return None;
    }
    Some(parse_command(name, &args[1..]))
}

fn parse_command(name: &str, rest: &[String]) -> Result<LocalCommand, String> {
    let mut workspace: Option<PathBuf> = None;
    let mut service: Option<String> = None;
    let mut json = false;
    let mut detach = false;
    let mut recheck = false;
    let mut positional: Vec<String> = Vec::new();

    let mut index = 0;
    while index < rest.len() {
        match rest[index].as_str() {
            "--workspace" | "-w" => {
                index += 1;
                let value = rest
                    .get(index)
                    .ok_or_else(|| "--workspace needs a directory".to_string())?;
                workspace = Some(PathBuf::from(value));
            }
            "--service" => {
                index += 1;
                let value = rest
                    .get(index)
                    .ok_or_else(|| "--service needs a service name".to_string())?;
                service = Some(value.clone());
            }
            "--json" => json = true,
            "--recheck" => recheck = true,
            // Removed in carrick#1008, and refused by name for one release
            // (carrick#1011): `carrick index` IS the inferred scan now.
            // Accepting the flag and ignoring it would hand a scaffold that
            // still types it a run it cannot tell apart from the old one.
            "--infer" => {
                return Err(
                    "`--infer` is gone: `carrick index` is the inferred scan itself. Run \
                     `carrick index`, with `--detach` when the shell may time out first."
                        .to_string(),
                );
            }
            "--detach" => detach = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option for `carrick {name}`: {other}"));
            }
            other => positional.push(other.to_string()),
        }
        index += 1;
    }

    // A hook-driven command that accepted `--detach` and ran in the foreground
    // anyway would be a promise nobody kept.
    if detach && name != "index" {
        return Err(format!("unknown option for `carrick {name}`: --detach"));
    }
    // `touch` states no verdict, so re-judging for it would buy a scan and
    // print nothing new.
    if recheck && name != "check" {
        return Err(format!("unknown option for `carrick {name}`: --recheck"));
    }

    match name {
        "derive" => {
            if workspace.is_none() {
                workspace = positional.first().map(PathBuf::from);
            }
            Ok(LocalCommand::Derive { workspace })
        }
        "index" => {
            // `carrick index <dir>` reads the same as `--workspace <dir>`;
            // both name the folder holding the repos.
            if workspace.is_none()
                && let Some(first) = positional.first()
            {
                workspace = Some(PathBuf::from(first));
            }
            Ok(LocalCommand::Index { workspace, detach })
        }
        "refresh" => Ok(LocalCommand::Refresh { service, workspace }),
        "status" => {
            if workspace.is_none()
                && let Some(first) = positional.first()
            {
                workspace = Some(PathBuf::from(first));
            }
            Ok(LocalCommand::Status { workspace, json })
        }
        "touch" | "check" => {
            let file = positional
                .first()
                .ok_or_else(|| format!("`carrick {name}` needs a file path"))?;
            let file = PathBuf::from(file);
            if name == "touch" {
                Ok(LocalCommand::Touch {
                    file,
                    workspace,
                    json,
                })
            } else {
                Ok(LocalCommand::Check {
                    file,
                    workspace,
                    json,
                    recheck,
                })
            }
        }
        _ => unreachable!("the caller matched the name"),
    }
}

/// Run a local command. The returned code is the process exit code: what a
/// read-only command FOUND never moves it — nothing local mode says blocks
/// anything — and the one exception is `check` refusing to answer at all,
/// which a script has to be able to tell from a clean verdict (carrick#1023
/// item 2, [`read`]).
pub fn run(command: LocalCommand) -> i32 {
    match command {
        LocalCommand::Derive { workspace } => match derive(workspace.as_deref()) {
            Ok(proposal) => {
                println!("{proposal}");
                0
            }
            Err(error) => {
                eprintln!("carrick derive: {error}");
                1
            }
        },
        LocalCommand::Index {
            workspace,
            detach: true,
        } => match start_detached(workspace.as_deref()) {
            Ok(()) => 0,
            Err(message) => {
                eprintln!("carrick index: {message}");
                1
            }
        },
        // Always inferred: the index a developer reads is the one the model
        // classified, and a facts-only form of this command had no user
        // (carrick#1008). `refresh` is the pass that runs no model.
        LocalCommand::Index { workspace, .. } => match build(workspace.as_deref(), None, true) {
            Ok(()) => 0,
            Err(message) => {
                eprintln!("carrick index: {message}");
                1
            }
        },
        LocalCommand::Refresh { service, workspace } => {
            // Never inferred. `refresh` runs from a session-start hook, and a
            // hook that spends real money every time an editor opens is not a
            // feature.
            match build(workspace.as_deref(), service.as_deref(), false) {
                Ok(()) => 0,
                Err(message) => {
                    eprintln!("carrick refresh: {message}");
                    1
                }
            }
        }
        LocalCommand::Touch {
            file,
            workspace,
            json,
        } => read(
            &file,
            workspace.as_deref(),
            json,
            Mode::Touch,
            Freshness::Indexed,
        ),
        LocalCommand::Check {
            file,
            workspace,
            json,
            recheck,
        } => read(
            &file,
            workspace.as_deref(),
            json,
            Mode::Check,
            if recheck {
                Freshness::Recheck
            } else {
                Freshness::Indexed
            },
        ),
        LocalCommand::Status { workspace, json } => status(workspace.as_deref(), json),
    }
}

fn derive(root: Option<&Path>) -> Result<serde_json::Value, String> {
    let root = root
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .ok_or("Could not locate the working directory")?;
    let workspace = Workspace::load(&root)?;
    let mut repos = Vec::new();
    for repo in &workspace.repos {
        let derived = crate::service_derivation::resolve(repo)?;
        // `services` carries the manifest facts that decide application from
        // library beside each service; `config` stays the carrick.json
        // skeleton, which those facts are not part of (carrick#994).
        // `warnings` is what this derivation found and a caller prints;
        // `notes` is the standing advice about a proposal of this shape, which
        // rides in this document and nowhere else (carrick#1032).
        repos.push(serde_json::json!({ "path": repo, "reason": derived.reason, "services": derived.service_documents(), "config": derived.config, "warnings": derived.warnings, "notes": derived.notes }));
    }
    Ok(
        serde_json::json!({ "schema": "carrick.derive/0", "workspace": workspace.root, "repos_detected_by": workspace.repos_detected_by, "repos_added": workspace.repos_added, "repos_excluded": workspace.repos_excluded, "missing": workspace.missing, "parent_proposal": workspace.parent_proposal, "repos": repos }),
    )
}

/// `status`: the workspace, with no file in the question.
///
/// A scan may be running while this is asked — that is the ordinary case a
/// minute after `carrick index --detach`, and the only surface that
/// can say so (carrick#992). It is reported first, and before the index is
/// read at all: the first detached scan of a workspace is answering the very
/// question "is anything happening", and at that moment there is no index.
fn status(root: Option<&Path>, json: bool) -> i32 {
    let Some(root) = super::workspace::locate(root, None) else {
        return report(
            ReadFailure::detailed(ReadError::NotIndexed, no_index_here(root, &[])),
            json,
            super::contract::STATUS_SCHEMA,
        );
    };
    let index_dir = root.join(super::workspace::INDEX_DIR);
    let scans = super::scan_state::read_all(&index_dir);
    // Read beside the scans and before the index, for the same reason: a run
    // killed before it wrote an index still uploaded the repos it got through,
    // and a reader parsing `--json` is answered about those either way
    // (carrick#995).
    let last_scan =
        crate::scan_spend::RunSpend::read(&super::workspace::last_scan_file(&index_dir));
    match super::query::status(&root) {
        Ok(mut output) => {
            output.running_scans = scans;
            output.last_scan = last_scan;
            if json {
                match serde_json::to_string_pretty(&output) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("carrick: could not serialize the answer: {e}");
                        return report(
                            ReadFailure::new(ReadError::IndexUnreadable),
                            json,
                            super::contract::STATUS_SCHEMA,
                        );
                    }
                }
            } else {
                print!("{}", output.render());
            }
            0
        }
        Err(error) => {
            // No index yet, or one this binary cannot read — and possibly a
            // scan building it as we speak. The scan is the answer to "what is
            // happening"; the error is the answer to "what is there". A reader
            // that asked for JSON gets both in the body and nothing else on
            // stdout: one line of prose in front of it is an unparseable
            // answer.
            if !json {
                for line in super::scan_state::status_lines(&scans, &index_dir) {
                    println!("{line}");
                }
            }
            // `status` takes no file and cannot be told to run a scan that is
            // already running: the refusal is rewritten here rather than in
            // `ReadError`, whose sentence belongs to `touch` and `check`
            // (carrick#1023 item 1).
            let error = if error.error == ReadError::NotIndexed {
                ReadFailure::detailed(ReadError::NotIndexed, no_index_here(Some(&root), &scans))
            } else {
                error
            };
            report_with_scans(
                error,
                json,
                super::contract::STATUS_SCHEMA,
                scans,
                last_scan,
            )
        }
    }
}

/// `index` and `refresh`: scan, join, write, and print the map.
///
/// `infer` makes each repo's scan a laptop scan: the model classifies what the
/// deterministic passes could not, the result is uploaded, and the same
/// payload is written to this build's cache directory so the read model comes
/// from the run that produced it (carrick#956 §8.3). `index` always infers;
/// `refresh` never does, which is the whole difference between them.
fn build(root: Option<&Path>, service: Option<&str>, infer: bool) -> Result<(), String> {
    let workspace = Workspace::load(&resolve_root(root)?)?;
    // Before anything is printed about a scan that is not going to happen.
    if infer && let Some(refusal) = inference_refusal(&workspace.repos) {
        return Err(refusal);
    }
    // The paid build records where it is, so `carrick status` can answer for
    // it and a scan that is killed leaves evidence rather than silence
    // (carrick#992). Detached or not: a user who runs `carrick index` in one
    // terminal and asks `carrick status` in another was told to start the
    // scan that was already running (carrick#1132). `refresh` records nothing;
    // it is the session-start hook's build and pays for nothing.
    if infer {
        super::scan_state::begin(
            &workspace.index_dir(),
            &super::scan_state::scan_id(),
            &workspace.root,
            infer,
        );
    }
    let outcome = build_workspace(&workspace, service, infer);
    // An index has just been written, so every record of a scan that is over
    // describes an older world — including the failed one a re-run was ordered
    // because of, which `carrick status` was still leading with (carrick#1023
    // item 13). Before this build's own record is finished, so that record
    // survives for the poll the scaffold tells an agent to run.
    //
    // Only the paid pass sweeps. `refresh` runs from the session-start hook,
    // and a hook that fires when an editor opens must not be the thing that
    // erases the only account of why a paid scan failed.
    if infer && outcome.is_ok() {
        super::scan_state::forget_superseded(&workspace.index_dir());
    }
    if infer {
        super::scan_state::finish(outcome.as_ref().err().map(String::as_str));
    }
    outcome
}

/// A signal is ending the build this process is running: close its scan
/// record with the signal as the reason (carrick#1132). The listener is the
/// binary's, beside the scan path's own; a build that records no scan has
/// nothing to close.
pub fn interrupted(signal: &str) {
    super::scan_state::interrupted(signal);
}

/// Where a build acts, before anything is loaded from it.
///
/// Build detection starts where the user asked. A parent's existing index is
/// useful to read commands, but must not widen this build's repo list.
fn resolve_root(root: Option<&Path>) -> Result<PathBuf, String> {
    root.map(Path::to_path_buf)
        .or_else(|| {
            std::env::var(super::workspace::WORKSPACE_ENV)
                .ok()
                .filter(|root| !root.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| "Could not locate the working directory".to_string())
}

/// The build itself: say what is about to happen, do it, print the map.
fn build_workspace(
    workspace: &Workspace,
    service: Option<&str>,
    infer: bool,
) -> Result<(), String> {
    if let Some(proposal) = &workspace.parent_proposal {
        eprintln!("carrick: {}", proposal.description());
    }
    eprintln!(
        "carrick: indexing {} repos ({}): {}",
        workspace.repos.len(),
        workspace.repos_detected_by,
        workspace
            .repos
            .iter()
            .map(|repo| repo.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for missing in &workspace.missing {
        eprintln!(
            "carrick: {} lists '{missing}', which is not a directory on this machine — it is \
             not indexed, so nothing will be said about it",
            super::workspace::WORKSPACE_FILE
        );
    }

    if infer {
        eprintln!(
            "carrick: this scan asks Carrick Cloud to classify what the deterministic passes \
             could not, and uploads the result. It runs the analysis; later scans read it."
        );
    }
    let outcome = super::index::run(workspace, service, infer)?;
    // A project whose services all carry the hash the cached blobs already
    // hold is not downloaded again, and a download that failed also leaves the
    // cached blobs in place. Saying how many rows each of those was is what
    // tells a skipped download apart from a failed one (carrick#1012 item 2).
    if let Some(line) = &outcome.hosted_download {
        eprintln!("carrick: {line}");
    }
    print_map(&outcome);
    Ok(())
}

/// `index --detach`: start the build in its own session and answer at once.
///
/// Everything that can refuse this build refuses it HERE, in the process the
/// user is watching: an unreadable workspace and a repo with no `carrick.json`
/// are answers, and an answer nobody reads because it went to a log file is
/// not one. What goes to the log is the scan.
///
/// The child is put in a session of its own, so that killing the shell that
/// started it — which is what an agent's tool timeout does — does not take the
/// scan with it. That is the whole point of the flag (carrick#992).
fn start_detached(root: Option<&Path>) -> Result<(), String> {
    let workspace = Workspace::load(&resolve_root(root)?)?;
    if let Some(refusal) = inference_refusal(&workspace.repos) {
        return Err(refusal);
    }
    let index_dir = workspace.index_dir();
    std::fs::create_dir_all(&index_dir).map_err(|e| format!("{}: {e}", index_dir.display()))?;
    super::workspace::write_self_ignore(&index_dir)
        .map_err(|e| format!("could not write the .carrick/.gitignore: {e}"))?;

    // The child inherits this run's id below, and records itself under its
    // head, so the id printed here is the one `carrick status` will name.
    let scan_id = super::scan_state::scan_id();
    let log = super::scan_state::log_file(&index_dir, &scan_id);
    let handle = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not find the carrick binary to run the scan with: {e}"))?;

    let mut command = std::process::Command::new(exe);
    command
        .arg("index")
        .arg("--workspace")
        .arg(&workspace.root)
        // One run id across the parent, the detached build and every scan it
        // drives, and the log says which of them is writing (carrick#997). It
        // is also what the child's scan id is taken from.
        .env(crate::logging::RUN_ID_ENV, crate::logging::run_id())
        .env(
            crate::logging::RUN_PHASE_ENV,
            format!("detached build {scan_id}"),
        )
        // Everything this child prints goes into a log file that nothing
        // renders, so it is asked for text rather than terminal control
        // sequences (carrick#1023 item 6).
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .stdin(std::process::Stdio::null())
        .stdout(
            handle
                .try_clone()
                .map_err(|e| format!("could not write to {}: {e}", log.display()))?,
        )
        .stderr(handle);
    detach_process(&mut command);

    let child = command
        .spawn()
        .map_err(|e| format!("could not start the scan: {e}"))?;
    println!(
        "scan {scan_id} started in the background (pid {}).",
        child.id()
    );
    println!("  watch it:  tail -f {}", log.display());
    println!(
        "  or:        carrick status --workspace {}",
        workspace.root.display()
    );
    println!(
        "The scan keeps running after this shell closes. `carrick status` names the service it \
         is on, how far through it is, and how long it has been running."
    );
    Ok(())
}

/// Put the child in a session (Unix) or a process group (Windows) of its own.
///
/// A tool that times out kills its process GROUP, and a child that shares one
/// with the shell dies with it — with the cloud's in-flight slot for that repo
/// held until its TTL and nothing uploaded.
#[cfg(unix)]
fn detach_process(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is async-signal-safe and is called in the child between
    // fork and exec, which is the only place this closure runs.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach_process(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS: no console of its own, and none inherited.
    // CREATE_NEW_PROCESS_GROUP: a Ctrl-C in the starting console is not
    // delivered to it.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Why this workspace may not be indexed yet, if it may not.
///
/// `carrick index` is the paid scan and it is meant to run once, so it runs
/// against a configuration a person has read rather than one a structural pass
/// guessed (ruling in carrick-cloud#799). `carrick init` no longer writes
/// `carrick.json`: it derives the proposal into `.carrick/proposal.json` and
/// prints the prompt that has an agent turn it into a config. A repo that has
/// not been through that step would be scanned with its service boundaries
/// unstated, its env vars undeclared, and no second free attempt.
///
/// The refusal names the two things that stand between this machine and the
/// scan — the config, and the credential — and nothing else. It used to point
/// at a facts-only pass as a rehearsal; there is no such step in the flow
/// (carrick#1008, cloud#832), so offering one here would invent a first run
/// that the scaffold prompt does not describe.
fn inference_refusal(repos: &[PathBuf]) -> Option<String> {
    let missing: Vec<&PathBuf> = repos
        .iter()
        .filter(|repo| std::fs::symlink_metadata(repo.join("carrick.json")).is_err())
        .collect();
    if missing.is_empty() {
        return None;
    }
    let named = missing
        .iter()
        .take(5)
        .map(|repo| repo.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let rest = missing.len().saturating_sub(5);
    Some(format!(
        "no carrick.json in {named}{}, and `carrick index` is the scan that builds the \
         index, so it runs after the config exists. `carrick init` wrote the derived services to \
         .carrick/proposal.json and printed the prompt that has an agent turn it into \
         carrick.json; read it back against the repo, run `carrick login` if this machine \
         is not signed in, and run this command once.",
        if rest > 0 {
            format!(" and {rest} more")
        } else {
            String::new()
        }
    ))
}

/// The map a build prints: every service, what it holds, and what it could not
/// classify.
fn print_map(outcome: &super::index::IndexOutcome) {
    let index = &outcome.index;
    println!();
    println!(
        "indexed {} repo(s) in {:.1}s at {}",
        outcome.scanned.len(),
        outcome.elapsed_secs,
        index.indexed_at
    );
    for repo in &index.repos {
        for service in &repo.services {
            // `0 route(s) 0 call(s)` is the same table cell for a service with
            // no API in it and for one whose every candidate is waiting for
            // the paid scan. The third number is what tells them apart
            // (carrick#997 item 8).
            let waiting = service
                .boundary
                .as_ref()
                .and_then(|boundary| boundary.awaiting_model())
                .map(|sentence| format!("  {sentence}"))
                .unwrap_or_default();
            println!(
                "  {:<28} {:>4} route(s)  {:>4} call(s)  {}{}",
                service.name,
                service.routes,
                service.calls,
                &service.commit[..service.commit.len().min(7)],
                waiting
            );
        }
    }
    let counterparts: usize = index
        .repos
        .iter()
        .flat_map(|repo| repo.files.values())
        .flat_map(|items| items.iter())
        .map(|item| item.counterparts.len())
        .sum();
    println!("  {counterparts} counterpart link(s) across the workspace");
    println!();
    for repo in &index.repos {
        for service in &repo.services {
            // The same renderer the read-only commands print from, so the
            // map, the terminal and a hook all say one sentence.
            let note =
                super::query::enrichment_note(&service.enrichment, service.boundary.as_ref());
            for line in
                super::query::boundary_lines(&service.name, &note, service.boundary.as_ref())
            {
                println!("{line}");
            }
        }
    }
}

/// What `carrick status` says when it found no index to read.
///
/// [`ReadError::NotIndexed`]'s own sentence is written for `touch` and
/// `check`, which take a file; this command takes none, and a workspace with a
/// scan in flight would be told to start the scan that is already running —
/// which is what a first paid run reads a minute after starting it
/// (carrick#1023 item 1). The scan's own line is printed above this one, so
/// the sentence points at it rather than repeating its id.
fn no_index_here(root: Option<&Path>, scans: &[super::scan_state::ScanState]) -> String {
    if scans.iter().any(|scan| scan.is_running()) {
        return "no index in this workspace yet: the scan above is still building it. Ask again \
                when it says it finished."
            .to_string();
    }
    match root {
        Some(root) => format!(
            "no index in {}. Run `carrick index --workspace {}` in the folder holding your repos.",
            root.join(super::workspace::INDEX_DIR).display(),
            root.display()
        ),
        None => "no index above this directory. Run `carrick index --workspace <dir>` in the \
                 folder holding your repos."
            .to_string(),
    }
}

/// `touch` and `check`: answer about one file.
///
/// A refusal from `check` is the one read that exits non-zero. `check` is the
/// command a script runs to ask whether this file's contracts hold, and "no
/// contract problems" and "there is no index" were the same exit code, so
/// nothing downstream could tell an answer from the absence of one
/// (carrick#1023 item 2). A VERDICT never moves the exit code — the command is
/// advisory and nothing blocks — and `touch`, the editor's read, still exits 0
/// whatever it finds, because an edit must never fail on a missing index.
fn read(file: &Path, root: Option<&Path>, json: bool, mode: Mode, freshness: Freshness) -> i32 {
    let refused = match mode {
        Mode::Check => 1,
        Mode::Touch => 0,
    };
    let refuse = |failure: ReadFailure| -> i32 {
        report(failure, json, super::contract::SCHEMA);
        refused
    };
    let Some(root) = super::workspace::locate(root, Some(file)) else {
        return refuse(ReadFailure::new(ReadError::NotIndexed));
    };
    match super::query::answer(&root, file, mode, freshness) {
        Ok(output) => {
            if json {
                match serde_json::to_string_pretty(&output) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("carrick: could not serialize the answer: {e}");
                        return refuse(ReadFailure::new(ReadError::IndexUnreadable));
                    }
                }
            } else {
                print!("{}", output.render());
            }
            0
        }
        Err(error) => refuse(error),
    }
}

/// Say why there is no answer, in the form the caller asked for, and answer 0.
/// A caller that has decided a refusal of its own is worth an exit code says
/// so itself; see [`read`].
fn report(failure: ReadFailure, json: bool, schema: &str) -> i32 {
    report_with_scans(failure, json, schema, Vec::new(), None)
}

/// The same, carrying any scan that is building the thing the caller asked
/// for: "there is no index" and "one is being built right now" are different
/// answers, and a reader that only gets the first will start a second scan.
fn report_with_scans(
    failure: ReadFailure,
    json: bool,
    schema: &str,
    scans: Vec<super::scan_state::ScanState>,
    last_scan: Option<crate::scan_spend::RunSpend>,
) -> i32 {
    // One sentence, on stderr and on the wire: the surfaces that read the JSON
    // body could not say what the terminal says until this carried it
    // (carrick#1009).
    eprintln!("carrick: {}", failure.message());
    if json {
        let body = ErrorOutput::new(&failure, schema)
            .with_scans(scans)
            .with_last_scan(last_scan);
        if let Ok(text) = serde_json::to_string(&body) {
            println!("{text}");
        }
    }
    0
}

fn print_help() {
    eprintln!(
        r#"Carrick — read-only facts from your disk

USAGE:
    carrick derive  [--workspace <dir>] --json
    carrick index   [--workspace <dir>] [--detach]
    carrick status  [--workspace <dir>] [--json]
    carrick touch   <file> [--workspace <dir>] [--json]
    carrick check   <file> [--workspace <dir>] [--json] [--recheck]
    carrick refresh [--service <name>] [--workspace <dir>]

    derive     Print the workspace and service proposal without writing files.
    --detach on `index` starts the build in the background and answers at once
    with a scan id. Its output goes to <workspace>/.carrick/scan-<id>.log and
    `carrick status` names the service it is on, how far through it is and how
    long it has been running. Use it when the shell running the command has a
    timeout shorter than the scan: a first scan of a mid-sized monorepo takes
    about fifteen minutes.

    index      Detect repositories, apply optional workspace overrides and
               write <dir>/.carrick/. Carrick Cloud classifies what the
               deterministic passes could not and the result is uploaded. It
               needs a carrick.json in every repo, which it refuses without,
               and a machine `carrick login` has signed in.
    status     What the workspace holds: every service, the commit it was
               indexed at, how far its repo has moved since, and its boundary.
    touch      The routes and calls in one file, and their counterparts in
               every other repo in the workspace. Reads the index only.
    check      The same, plus the contract verdicts the index already holds.
               --recheck re-extracts the file and re-judges it against the
               index before answering, when the tree has moved past the index:
               one repo is re-scanned and the join runs over blobs already on
               disk, with no model and nothing on the wire. It has ten seconds
               (CARRICK_RECHECK_BUDGET_MS); past that the answer is the indexed
               one and says so.
    refresh    Re-scan one service (or every repo) and re-join.

The workspace is a repository or the folder holding its sibling repositories.
Optional carrick-workspace.json overrides add paths with `repos` and remove
names with `exclude`. `touch` and `check` find an index above the file, or
take its workspace from --workspace or CARRICK_WORKSPACE.

Output shape: docs/local-mode-output.md (`--json` prints carrick.check/0, or
carrick.status/0 from `status`)."#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(input: &[&str]) -> Vec<String> {
        input.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn status_takes_a_workspace_and_the_json_flag() {
        let parsed = parse(&args(&["status", "--json"])).unwrap().unwrap();
        assert_eq!(
            parsed,
            LocalCommand::Status {
                workspace: None,
                json: true,
            }
        );
    }

    /// The paid scan runs once, so it runs against a config someone has read.
    /// A repo that has not been through the scaffold step is named, and the
    /// refusal says what to do instead of it (carrick-cloud#799). Since
    /// carrick#1008 this is `carrick index`'s own refusal, not a flag's.
    #[test]
    fn the_index_is_refused_until_every_repo_has_a_config() {
        let workspace = tempfile::tempdir().unwrap();
        let configured = workspace.path().join("api");
        let bare = workspace.path().join("web");
        std::fs::create_dir_all(&configured).unwrap();
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(configured.join("carrick.json"), "{}").unwrap();

        assert_eq!(inference_refusal(std::slice::from_ref(&configured)), None);
        let refusal = inference_refusal(&[configured.clone(), bare.clone()]).unwrap();
        assert!(
            refusal.contains(&bare.display().to_string()),
            "the repo without a config is named: {refusal}"
        );
        assert!(
            !refusal.contains(&configured.display().to_string()),
            "the configured repo is not: {refusal}"
        );
        assert!(refusal.contains(".carrick/proposal.json"), "{refusal}");
        assert!(refusal.contains("carrick index"), "{refusal}");
        // The two things that stand between this machine and the scan, and
        // nothing else: there is no rehearsal pass in the flow (cloud#832).
        assert!(refusal.contains("carrick login"), "{refusal}");
        assert!(!refusal.contains("carrick refresh"), "{refusal}");
    }

    /// A long list of unconfigured repos is a folder someone pointed at, not a
    /// screen of paths.
    #[test]
    fn the_refusal_caps_the_repos_it_names() {
        let workspace = tempfile::tempdir().unwrap();
        let repos: Vec<PathBuf> = (0..8)
            .map(|n| workspace.path().join(format!("r{n}")))
            .collect();
        let refusal = inference_refusal(&repos).unwrap();
        assert!(refusal.contains("and 3 more"), "{refusal}");
        assert!(!refusal.contains("r7"), "{refusal}");
    }

    #[test]
    fn a_scan_path_is_not_a_local_command() {
        // `carrick .` and `carrick /path/to/repo` are the CI scan and must
        // keep reaching it.
        assert!(parse(&args(&["."])).is_none());
        assert!(parse(&args(&["/repos/api", "--no-cache"])).is_none());
        assert!(parse(&args(&[])).is_none());
    }

    #[test]
    fn touch_reads_a_file_and_the_json_flag() {
        let parsed = parse(&args(&["touch", "src/app.ts", "--json"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            parsed,
            LocalCommand::Touch {
                file: PathBuf::from("src/app.ts"),
                workspace: None,
                json: true,
            }
        );
    }

    #[test]
    fn check_takes_the_workspace_a_hook_passes_it() {
        let parsed = parse(&args(&["check", "src/app.ts", "--workspace", "/w"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            parsed,
            LocalCommand::Check {
                file: PathBuf::from("src/app.ts"),
                workspace: Some(PathBuf::from("/w")),
                json: false,
                recheck: false,
            }
        );
    }

    #[test]
    fn index_takes_the_workspace_as_a_positional_too() {
        let parsed = parse(&args(&["index", "/w"])).unwrap().unwrap();
        assert_eq!(
            parsed,
            LocalCommand::Index {
                workspace: Some(PathBuf::from("/w")),
                detach: false,
            }
        );
    }

    /// `--infer` is gone, and it is refused by name rather than ignored: a
    /// scaffold or a transcript that still types it asked for something this
    /// binary no longer distinguishes, and silence would read as agreement
    /// (carrick#1008). Delete this with the flag's last release (carrick#1011).
    #[test]
    fn the_removed_infer_flag_is_refused_by_name() {
        for command in ["index", "refresh"] {
            let error = parse(&args(&[command, "--infer"])).unwrap().unwrap_err();
            assert!(
                error.contains("`--infer` is gone"),
                "the flag is named as removed, not as unknown: {error}"
            );
            assert!(
                error.contains("carrick index"),
                "and the command that replaces it is named: {error}"
            );
            assert!(
                !error.contains("carrick refresh"),
                "and no first-run copy sends anyone to the pass that runs no \
                 model (cloud#832): {error}"
            );
        }
    }

    /// The flag the ruled first run needs, on the one command that can take
    /// minutes. `refresh` runs from a hook and has no shell to outlive, so it
    /// refuses the flag rather than accepting it and running in the foreground
    /// (carrick#992).
    #[test]
    fn a_scan_can_be_detached_from_the_shell_that_starts_it() {
        assert_eq!(
            parse(&args(&["index", "/w", "--detach"])).unwrap().unwrap(),
            LocalCommand::Index {
                workspace: Some(PathBuf::from("/w")),
                detach: true,
            }
        );
        assert_eq!(
            parse(&args(&["index", "--detach"])).unwrap().unwrap(),
            LocalCommand::Index {
                workspace: None,
                detach: true,
            }
        );
        assert_eq!(
            parse(&args(&["refresh", "--detach"])).unwrap(),
            Err("unknown option for `carrick refresh`: --detach".to_string())
        );
        assert_eq!(
            parse(&args(&["status", "--detach"])).unwrap(),
            Err("unknown option for `carrick status`: --detach".to_string())
        );
    }

    #[test]
    fn refresh_names_one_service() {
        let parsed = parse(&args(&["refresh", "--service", "api"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            parsed,
            LocalCommand::Refresh {
                service: Some("api".to_string()),
                workspace: None,
            }
        );
    }

    #[test]
    fn a_file_is_required_to_touch() {
        let error = parse(&args(&["touch"])).unwrap().unwrap_err();
        assert!(error.contains("needs a file path"), "{error}");
    }
}
