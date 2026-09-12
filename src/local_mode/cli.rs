//! The local subcommands: parsing, dispatch, and what each one prints.
//!
//! `carrick <path>` is still the scan every CI run performs. These four names
//! are checked before that, and only as the first argument, so the only path
//! they take from the old CLI is a directory literally named `index`, `touch`,
//! `check` or `refresh`.

use std::path::{Path, PathBuf};

use super::contract::{ErrorOutput, ReadError};
use super::query::Mode;
use super::workspace::Workspace;

/// A local command, once its arguments have been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalCommand {
    /// Read the shared workspace/service proposal without scanning or writing.
    Derive { workspace: Option<PathBuf> },
    /// Scan every repo the workspace lists, and build the index.
    Index {
        workspace: Option<PathBuf>,
        /// Ask Carrick Cloud to classify what the deterministic passes could
        /// not, and upload the result.
        ///
        /// Off by default and deliberately so: a local index costs nothing and
        /// runs from a hook, and inference is paid analysis. It is refused
        /// outright until every repo in the workspace has a `carrick.json`,
        /// because the one paid scan runs against a config someone has read
        /// (see [`inference_refusal`]).
        infer: bool,
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
    let mut infer = false;
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
            "--infer" => infer = true,
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

    // Only `index` infers. Accepting the flag elsewhere and ignoring it would
    // let someone ask a hook-driven command to do the paid thing and believe
    // it had.
    if infer && name != "index" {
        return Err(format!("unknown option for `carrick {name}`: --infer"));
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
            Ok(LocalCommand::Index { workspace, infer })
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
                })
            }
        }
        _ => unreachable!("the caller matched the name"),
    }
}

/// Run a local command. The returned code is the process exit code: a
/// read-only command always answers 0, whatever it found, so an editor hook
/// cannot fail an edit.
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
        LocalCommand::Index { workspace, infer } => {
            match build(workspace.as_deref(), None, infer) {
                Ok(()) => 0,
                Err(message) => {
                    eprintln!("carrick index: {message}");
                    1
                }
            }
        }
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
        } => read(&file, workspace.as_deref(), json, Mode::Touch),
        LocalCommand::Check {
            file,
            workspace,
            json,
        } => read(&file, workspace.as_deref(), json, Mode::Check),
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
        repos.push(serde_json::json!({ "path": repo, "reason": derived.reason, "services": derived.service_documents(), "config": derived.config, "warnings": derived.warnings }));
    }
    Ok(
        serde_json::json!({ "schema": "carrick.derive/0", "workspace": workspace.root, "repos_detected_by": workspace.repos_detected_by, "repos_added": workspace.repos_added, "repos_excluded": workspace.repos_excluded, "missing": workspace.missing, "parent_proposal": workspace.parent_proposal, "repos": repos }),
    )
}

/// `status`: the workspace, with no file in the question.
fn status(root: Option<&Path>, json: bool) -> i32 {
    let Some(root) = super::workspace::locate(root, None) else {
        return report(ReadError::NotIndexed, json, super::contract::STATUS_SCHEMA);
    };
    match super::query::status(&root) {
        Ok(output) => {
            if json {
                match serde_json::to_string_pretty(&output) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("carrick: could not serialize the answer: {e}");
                        return report(
                            ReadError::IndexUnreadable,
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
        Err(error) => report(error, json, super::contract::STATUS_SCHEMA),
    }
}

/// `index` and `refresh`: scan, join, write, and print the map.
///
/// `infer` makes each repo's scan a laptop scan: the model classifies what the
/// deterministic passes could not, the result is uploaded, and the same
/// payload is written to this build's cache directory so the read model comes
/// from the run that produced it (carrick#956 §8.3).
fn build(root: Option<&Path>, service: Option<&str>, infer: bool) -> Result<(), String> {
    // Build detection starts where the user asked. A parent's existing index
    // is useful to read commands, but must not widen this build's repo list.
    let root = root
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var(super::workspace::WORKSPACE_ENV)
                .ok()
                .filter(|root| !root.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok())
        .ok_or("Could not locate the working directory")?;
    let workspace = Workspace::load(&root)?;
    // Before anything is printed about a scan that is not going to happen.
    if infer && let Some(refusal) = inference_refusal(&workspace.repos) {
        return Err(refusal);
    }
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
             could not, and uploads the result. It is the paid analysis; later scans read it."
        );
    }
    let outcome = super::index::run(&workspace, service, infer)?;
    print_map(&outcome);
    Ok(())
}

/// Why this workspace may not be scanned with inference yet, if it may not.
///
/// The inferred scan is the paid one and it is meant to run once, so it runs
/// against a configuration a person has read rather than one a structural pass
/// guessed (ruling in carrick-cloud#799). `carrick init` no longer writes
/// `carrick.json`: it derives the proposal into `.carrick/proposal.json` and
/// prints the prompt that has an agent turn it into a config. A repo that has
/// not been through that step would be scanned with its service boundaries
/// unstated, its env vars undeclared, and no second free attempt.
///
/// The free `carrick index` is unaffected: it states what it can and marks the
/// rest unclassified, which is exactly the pass that proves a new config.
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
        "no carrick.json in {named}{}, and an inferred scan is the paid one, so it runs \
         after the config exists. `carrick init` wrote the derived services to \
         .carrick/proposal.json and printed the prompt that has an agent turn it into \
         carrick.json; then `carrick index` states what it can for nothing, and this \
         command is the scan that classifies the rest.",
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
            println!(
                "  {:<28} {:>4} route(s)  {:>4} call(s)  {}",
                service.name,
                service.routes,
                service.calls,
                &service.commit[..service.commit.len().min(7)]
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

/// `touch` and `check`: answer about one file.
fn read(file: &Path, root: Option<&Path>, json: bool, mode: Mode) -> i32 {
    let Some(root) = super::workspace::locate(root, Some(file)) else {
        return report(ReadError::NotIndexed, json, super::contract::SCHEMA);
    };
    match super::query::answer(&root, file, mode) {
        Ok(output) => {
            if json {
                match serde_json::to_string_pretty(&output) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("carrick: could not serialize the answer: {e}");
                        return report(ReadError::IndexUnreadable, json, super::contract::SCHEMA);
                    }
                }
            } else {
                print!("{}", output.render());
            }
            0
        }
        Err(error) => report(error, json, super::contract::SCHEMA),
    }
}

/// Say why there is no answer, in the form the caller asked for, and still
/// exit 0.
fn report(error: ReadError, json: bool, schema: &str) -> i32 {
    eprintln!("carrick: {}", error.message());
    if json {
        let body = ErrorOutput::new(error, schema);
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
    carrick index   [--workspace <dir>] [--infer]
    carrick status  [--workspace <dir>] [--json]
    carrick touch   <file> [--workspace <dir>] [--json]
    carrick check   <file> [--workspace <dir>] [--json]
    carrick refresh [--service <name>] [--workspace <dir>]

    derive     Print the workspace and service proposal without writing files.
    --infer on `index` asks Carrick Cloud to classify what the deterministic
    passes could not and uploads the result. It is the paid scan, so it needs
    a carrick.json in every repo and refuses without one. Off everywhere else,
    including `refresh`, which runs from a hook.

    index      Detect repositories, apply optional workspace overrides and
               write <dir>/.carrick/. No model runs on this machine.
    status     What the workspace holds: every service, the commit it was
               indexed at, how far its repo has moved since, and its boundary.
    touch      The routes and calls in one file, and their counterparts in
               every other repo in the workspace. Reads the index only.
    check      The same, plus the contract verdicts the index already holds.
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
    /// refusal says what to do instead of it (carrick-cloud#799).
    #[test]
    fn inference_is_refused_until_every_repo_has_a_config() {
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
                infer: false,
            }
        );
    }

    /// Inference is paid analysis, so it is asked for and never assumed. The
    /// command that runs from a session-start hook does not take the flag at
    /// all, which is why it is on `Index` and not on `Refresh`.
    #[test]
    fn inference_is_opt_in_on_index_and_unavailable_to_refresh() {
        let parsed = parse(&args(&["index", "/w", "--infer"])).unwrap().unwrap();
        assert_eq!(
            parsed,
            LocalCommand::Index {
                workspace: Some(PathBuf::from("/w")),
                infer: true,
            }
        );
        assert_eq!(
            parse(&args(&["refresh", "--infer"])).unwrap(),
            Err("unknown option for `carrick refresh`: --infer".to_string())
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
