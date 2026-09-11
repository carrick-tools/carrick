# carrick

Carrick indexes local TypeScript repositories and identifies the routes and
calls in the file you are editing, their counterparts in indexed repositories,
and whether their contracts agree. Configured editors show diagnostics, and
coding agents can receive context through supported hooks or explicit checks.

```
npm install -g carrick
carrick login
cd ~/code            # a repo, or the folder that holds your repos
carrick init
```

`carrick login` opens the browser to authorise a Carrick workspace. `init`
accepts that credential or a `CARRICK_TOKEN` environment override, and signs in
through the browser itself when a terminal has neither. GitHub CLI credentials
do not grant Carrick access.

Existing users can initialise the local workspace against a named Carrick
project:

```
carrick init --project payments
```

The command shows the current project assignment for every proposed GitHub
repo. When the project is not in the workspace yet, it lists the workspace's
projects and offers to create the named one from the terminal; where the API
has no such action it prints the project link instead. Repository assignment
stays in the browser, so the command prints the GitHub App and repo-assignment
links and, in an interactive terminal, opens the page. The CLI then reads
`resolve-repos` until every proposed repo reports `project_slug: "payments"`.
A repo connected to another project remains pending. In a noninteractive shell,
an unmet target prints the same links and exits with status 1 without writing
the local setup.

Without `--project` the project step still runs: the command takes the project
the repos are already in, and otherwise lists the workspace's projects for you
to name one, which it creates from the terminal where the API allows that. An
empty answer leaves the step to the browser.

Credentials live in `$XDG_CONFIG_HOME/carrick/credentials.json`, falling back
to `~/.config/carrick/` on macOS and Linux or `%APPDATA%\carrick\` on Windows.
The file is written with mode 0600. `carrick logout` removes it; unset
`CARRICK_TOKEN` separately if you set that override. Revoke issued keys at
[your account](https://app.carrick.tools/account).

Rust derives the same services for CI, local indexing and `init`. An existing
`carrick.json` is authoritative. When it is missing, `init` presents workspace
packages and their compiler configuration and writes that proposal to
`.carrick/proposal.json`, which is ignored. It writes no `carrick.json`: your
agent turns the proposal into one, so the config you commit is one somebody has
read, and the first scan runs against it. `carrick index` is free and states
what it can; `carrick index --infer` is the scan that asks Carrick to classify
the rest, and it refuses to run until a `carrick.json` exists.

Workspace detection handles a repo root or immediate sibling repositories.
An optional `carrick-workspace.json` adds paths through `repos` and removes
directory names through `exclude`; init preserves this file and does not
create one. `carrick derive --workspace . --json` previews the Rust proposal
without writing files or scanning.

Deno projects use their existing `deno.json` or `deno.jsonc` and require Deno
2.9.4 or newer on PATH. Before local indexing, prepare their dependencies with
`deno install --frozen --node-modules-dir=none` from the Deno workspace root;
this prevents npm lifecycle scripts from running even when `allowScripts`
authorizes them. Generate any application-owned declarations through the
project's normal build before indexing.

Deno services normally omit `tsconfig`. An explicit ordinary TypeScript config
selects the TypeScript path; an explicit Deno config must be the nearest Deno
manifest, with `deno.json` taking precedence over `deno.jsonc`. Import maps must
be local files.

## What it installs

The scanner is a Rust binary that arrives as
`@carrick-tools/cli-<platform>-<arch>`, an optional dependency npm resolves by
`os` and `cpu`; the type sidecar, the language server and the hook scripts come
with this package. Installing Carrick requires no postinstall script, so
`--ignore-scripts` works. Dependency preparation and type checking during indexing
can download registry packages; `index` and `refresh` can also download
authenticated hosted indexes.

Node 24 or newer is required because the sidecar resolves request and response
types in a Node process.

## The commands

| Command | What it does |
|---|---|
| `carrick login` | Authorise a Carrick workspace in the browser, or verify `CARRICK_TOKEN` |
| `carrick logout` | Remove the saved local credential |
| `carrick init [--project SLUG]` | The repo list, the project, the service proposal in `.carrick/`, the agent hooks, the MCP connection, and the prompt that writes `carrick.json` |
| `carrick index` | Derive the workspace, apply optional repo overrides and write `.carrick/` |
| `carrick refresh [--service X]` | Re-scan one repo, or all of them, and re-join |
| `carrick check <file>` | What the index knows about that file, verdicts included |
| `carrick touch <file>` | The same, without the verdicts |
| `carrick status` | What the index holds, and how far each repo has moved since |
| `carrick lsp --stdio` | The language server, for an editor or any LSP client |
| `carrick hook post-edit` | Claude Code PostToolUse hook, reading the tool payload on stdin |
| `carrick hook session-start` | Claude Code SessionStart hook |
| `carrick templates workflow` | Print the CI workflow to add to a repo |
| `carrick <path>` | The full scan, which is what the GitHub Action runs |

`check`, `touch` and `status` read `.carrick/` and write nothing. Add `--json`
for the machine-readable shape, pinned in
[`docs/local-mode-output.md`](https://github.com/carrick-tools/carrick/blob/main/docs/local-mode-output.md).

## Where the answers land

- **Claude Code**: the hooks `carrick init` writes deliver on the edit itself.
  `claude --plugin-dir <carrick checkout>/plugin` adds the language server too.
- **VS Code, Cursor, Windsurf**: the `carrick-tools.carrick` extension is a
  client on `carrick lsp --stdio` and publishes diagnostics in the Problems
  panel. Whether an editor-hosted agent reads those diagnostics depends on its
  integration and configuration.
- **Neovim, Helix, Zed, JetBrains**: any LSP client, on the same command.
- **Anything else**: `carrick check <file>` on demand, and the pull request
  check in CI.

## In your editor

Go to definition on a call to another service jumps to the handler that serves
it, in the other repo. On a route it lists the call sites that reach it, and
the editor shows the picker. Carrick answers only inside a row the index holds,
and only when the file on the other side is on this disk, so every other jump
falls through to TypeScript exactly as it did before.

## Settings

One switch per surface. In VS Code they are settings; any other LSP client
sends the same keys, with the `carrick.` prefix stripped, as
`initializationOptions`.

| Setting | Default | What it turns off |
|---|---|---|
| `carrick.binary` | the `carrick` on PATH | — the path to the CLI, not a surface |
| `carrick.diagnostics` | on | The verdicts in the Problems panel |
| `carrick.definition` | on | Cross-repo go to definition. Off means Carrick answers nothing and your other definition providers are untouched |
| `carrick.boundary` | on | The boundary: the status bar item, or the file-level row in a client without one |

`CARRICK_CHANNEL=off` in the environment turns off delivery altogether.

## What a local index holds

A local index holds deterministic routes, calls, protocol operations and their
types, together with eligible hosted model answers. `index` and `refresh` read
authenticated hosted indexes and replay answers for unchanged files when the
hosted commit is available locally and cache versions match. Changed files
keep their local facts while their hosted model answers are withheld. Boundary
rows report enrichment status and remaining unclassified candidates; no model
runs on the local machine.

Hosted queries cover connected, indexed repositories in the selected Carrick
project, including repositories absent from this disk, using their most recent
default-branch indexes. They are available over MCP, and `carrick init`
connects the agent clients it finds on the machine: Claude Code through
`claude mcp add`, and Cursor, Windsurf and VS Code by adding a `carrick` server
to the client's own configuration file, leaving every other entry in it alone.
A machine with none of them is given the line to run:

```
claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp
```

The first scan on a repository's default branch writes its hosted index. A
Claude Code session opened in a workspace still waiting for one starts
`carrick refresh` in the background, at most once an hour
(`CARRICK_REFRESH_COOLDOWN_MS` states another gap), so the hosted rows arrive
without a command to remember.

## Licence

Elastic License 2.0. See
[LICENSE.md](https://github.com/carrick-tools/carrick/blob/main/LICENSE.md).
