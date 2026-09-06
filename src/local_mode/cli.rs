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
    /// Scan every repo the workspace lists, and build the index.
    Index { workspace: Option<PathBuf> },
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

/// Read a local command from the argument list, or `None` when the first
/// argument is not one of the four names.
pub fn parse(args: &[String]) -> Option<Result<LocalCommand, String>> {
    let name = args.first()?.as_str();
    if !matches!(name, "index" | "touch" | "check" | "refresh") {
        return None;
    }
    Some(parse_command(name, &args[1..]))
}

fn parse_command(name: &str, rest: &[String]) -> Result<LocalCommand, String> {
    let mut workspace: Option<PathBuf> = None;
    let mut service: Option<String> = None;
    let mut json = false;
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

    match name {
        "index" => {
            // `carrick index <dir>` reads the same as `--workspace <dir>`;
            // both name the folder holding the repos.
            if workspace.is_none()
                && let Some(first) = positional.first()
            {
                workspace = Some(PathBuf::from(first));
            }
            Ok(LocalCommand::Index { workspace })
        }
        "refresh" => Ok(LocalCommand::Refresh { service, workspace }),
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
        LocalCommand::Index { workspace } => match build(workspace.as_deref(), None) {
            Ok(()) => 0,
            Err(message) => {
                eprintln!("carrick index: {message}");
                1
            }
        },
        LocalCommand::Refresh { service, workspace } => {
            match build(workspace.as_deref(), service.as_deref()) {
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
    }
}

/// `index` and `refresh`: scan, join, write, and print the map.
fn build(root: Option<&Path>, service: Option<&str>) -> Result<(), String> {
    let root = super::workspace::locate(root, None).ok_or_else(|| {
        format!(
            "no {} found here or above. Create one listing your repos, for example \
             {{\"repos\": [\"./api\", \"./web\"]}}",
            super::workspace::WORKSPACE_FILE
        )
    })?;
    let workspace = Workspace::load(&root)?;
    for missing in &workspace.missing {
        eprintln!(
            "carrick: {} lists '{missing}', which is not a directory on this machine — it is \
             not indexed, so nothing will be said about it",
            super::workspace::WORKSPACE_FILE
        );
    }

    let outcome = super::index::run(&workspace, service)?;
    print_map(&outcome);
    Ok(())
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
            let note = super::query::boundary_note(service.boundary.as_ref());
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
        return report(ReadError::NotIndexed, json);
    };
    match super::query::answer(&root, file, mode) {
        Ok(output) => {
            if json {
                match serde_json::to_string_pretty(&output) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("carrick: could not serialize the answer: {e}");
                        return report(ReadError::IndexUnreadable, json);
                    }
                }
            } else {
                print!("{}", output.render());
            }
            0
        }
        Err(error) => report(error, json),
    }
}

/// Say why there is no answer, in the form the caller asked for, and still
/// exit 0.
fn report(error: ReadError, json: bool) -> i32 {
    eprintln!("carrick: {}", error.message());
    if json {
        let body = ErrorOutput::new(error);
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
    carrick index   [--workspace <dir>]
    carrick touch   <file> [--workspace <dir>] [--json]
    carrick check   <file> [--workspace <dir>] [--json]
    carrick refresh [--service <name>] [--workspace <dir>]

    index      Scan every repo listed in <dir>/carrick-workspace.json into
               <dir>/.carrick/. Deterministic facts only: no model runs here.
    touch      The routes and calls in one file, and their counterparts in
               every other repo in the workspace. Reads the index only.
    check      The same, plus the contract verdicts the index already holds.
    refresh    Re-scan one service (or every repo) and re-join.

The workspace is the folder holding your repos and a carrick-workspace.json
listing them: {{"repos": ["./api", "./web"]}}. `touch` and `check` find it
above the file, or take it from --workspace or CARRICK_WORKSPACE.

Output shape: docs/local-mode-output.md (`--json` prints carrick.check/0)."#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(input: &[&str]) -> Vec<String> {
        input.iter().map(|s| s.to_string()).collect()
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
            }
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
