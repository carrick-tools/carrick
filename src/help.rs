//! What `carrick --help` prints.
//!
//! The binary owns the top-level help, including the environment block, and
//! the npm package's shim (`npm/carrick/bin/carrick.mjs`) appends the commands
//! it adds on top under the same group headings. A command the parser accepts
//! but this text does not name is a command nobody finds, which is how the
//! workspace commands stayed invisible from the release that shipped them
//! (carrick#708) to carrick#976, so the test at the bottom holds this text to
//! `local_mode::cli::LOCAL_COMMANDS`.

pub const HELP: &str = r#"Carrick keeps a live, type-aware, intent-aware index of every TypeScript
service in your GitHub org, so coding agents and editors can answer across
repos.

USAGE:
    carrick [OPTIONS] [REPO_PATH]   scan one repository and upload its index
    carrick <COMMAND> [OPTIONS]     build or read the index on this machine

SCAN:
    [REPO_PATH]    Repository to analyse (default: the working directory). This
                   is what the GitHub Action runs: it scans the source, resolves
                   the types, and uploads that repository's index.

WORKSPACE:
    init       Set up this folder: sign in, connect the repos, propose the
               services, and wire up your editor and agent. The command the
               first run starts with. Installed by the npm package.
    derive     Print the repos and services a folder resolves to, writing nothing.
    index      Scan every repo in the workspace and build <workspace>/.carrick/.
    refresh    Re-scan one service, or every repo, and re-join the index.
    status     Every service the workspace holds, the commit each was indexed at,
               how far its repo has moved since, and its boundary.
    check      The routes and calls in one file, their counterparts in every
               other repo, and the contract verdicts the index already holds.
    touch      The same without the verdicts, cheap enough for an editor to run
               it on every edit.

    The workspace is a repository, or the folder holding sibling repositories.
    Each command takes --workspace <dir>; check and touch take a file path and
    --json. `carrick <COMMAND> --help` prints the arguments of one of them.

OPTIONS:
    -h, --help     Print this help message
    -v, --verbose  Enable verbose (debug-level) terminal output
    --no-cache     Skip incremental cache and run a full analysis

ENVIRONMENT VARIABLES:
    ACTIONS_ID_TOKEN_REQUEST_URL    GitHub Actions OIDC token endpoint (auto-set
                                    when the job grants `id-token: write`)
    ACTIONS_ID_TOKEN_REQUEST_TOKEN  Bearer token for the OIDC endpoint (auto-set)
    CARRICK_TOKEN                   A Carrick API token, used in place of the
                                    credential `carrick login` saves
    CARRICK_WORKSPACE               The workspace the commands above act on,
                                    for a file or a working directory that is
                                    not under it
    CARRICK_MOCK_ALL                Use mock storage instead of Carrick Cloud
    CARRICK_API_ENDPOINT            API endpoint for the carrick service (build-time)
    CARRICK_INTENT_CONCURRENCY      Concurrent function-intent requests (default 8).
                                    Lower it if a large repo loses intents to
                                    backend overload; capped by
                                    CARRICK_CONCURRENCY_LIMIT
    CARRICK_ALLOW_PARTIAL_ANALYSIS  Upload and exit 0 even when files were not
                                    analysed. Off by default: a run that lost
                                    analyzer results is reported and fails,
                                    rather than overwriting the index with a
                                    thinner one
    CARRICK_SIDECAR_DIR             Directory holding the type sidecar's
                                    dist/src/index.js. Set by the npm package,
                                    where the binary and the sidecar install
                                    into different directories; a source
                                    checkout finds it without this
    CARRICK_SIDECAR_READY_TIMEOUT_SECS
                                    How long to wait for the type sidecar to
                                    build its TypeScript program (default 180).
                                    Raise it for a large monorepo whose
                                    dependencies are installed
    CARRICK_ALLOW_MISSING_TYPES     Scan and exit 0 even when the type sidecar
                                    never became ready. Off by default: such a
                                    run would index every endpoint with no
                                    request or response types
"#;

/// The commands the `carrick` npm package adds on top of this binary's own
/// (`npm/carrick/bin/carrick.mjs`). The binary cannot run them, and it must
/// still be able to say where they are: `init` is where the first run starts,
/// so "unknown command" is the wrong answer for it, and a user who reached
/// the binary directly needs the package named rather than the name denied.
pub const PACKAGE_COMMANDS: [&str; 6] = ["init", "login", "logout", "lsp", "hook", "templates"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_mode::cli::LOCAL_COMMANDS;

    #[test]
    fn every_command_the_parser_accepts_is_named_with_a_description() {
        for command in LOCAL_COMMANDS {
            let line = HELP
                .lines()
                .find(|line| line.trim_start().starts_with(&format!("{command} ")))
                .unwrap_or_else(|| panic!("`carrick {command}` is parsed but not named in --help"));
            assert!(
                line.trim_start().len() > command.len() + 8,
                "`carrick {command}` is named in --help with no description: {line}"
            );
        }
    }

    /// The command the first run starts with is named here, whichever half of
    /// the install a user reaches (carrick#997 item 5). It is the npm
    /// package's, so the text says so and the binary's unknown-command answer
    /// points at the package rather than denying the name.
    #[test]
    fn the_command_the_flow_starts_with_is_named_too() {
        let line = HELP
            .lines()
            .find(|line| line.trim_start().starts_with("init "))
            .expect("`carrick init` is named in --help");
        assert!(line.trim_start().len() > "init".len() + 8, "{line}");
        assert!(PACKAGE_COMMANDS.contains(&"init"));
    }

    #[test]
    fn the_environment_block_names_only_variables_the_source_reads() {
        // A documented variable nothing reads is worse than an undocumented
        // one: a user sets it and waits for behaviour that never comes.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = String::new();
        for entry in walkdir::WalkDir::new(&src)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "rs")
                && path.file_name().is_some_and(|name| name != "help.rs")
                && let Ok(text) = std::fs::read_to_string(path)
            {
                sources.push_str(&text);
            }
        }
        for name in HELP
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|word| word.starts_with("CARRICK_") || word.starts_with("ACTIONS_"))
        {
            assert!(
                sources.contains(name),
                "--help documents {name}, which nothing under src/ reads"
            );
        }
    }
}
