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

An installed `carrick` does not move on its own, and a scan on an old build can
fail on a defect that is already fixed. So every command reads a cached answer
to "is there a newer one", refreshes it in the background, and prints a line
naming the exact update command for the way this copy was installed. It never
blocks on the network, and `CARRICK_NO_UPDATE_CHECK=1` turns it off for a
reproducible run. The one thing it installs is an older **global** `carrick`
when you run through `npx`: that install answers your agent's hooks and every
new shell, so it is brought level with the version you just ran, using the
package manager that owns it and nothing that overrides your own npm
configuration. A machine with no global is never installed onto without a yes —
`carrick init` asks, and `carrick init --install-global` is the answer for a
script. In CI it warns
and continues, so a workflow decides its own version — and
`carrick-tools/carrick@v1` in a workflow already moves to each release.

`init` prints one line per thing it did — `◇` done, `▲` a warning with what to
do about it, `■` something it could not do and why — and ends on the sentence
to paste to your agent. What it installs, the hooks included, is in the
[CLI reference](https://docs.carrick.tools/cli#workspace-initialisation). The CI
check and a `carrick.json` written by hand are in
[Building the index](https://docs.carrick.tools/building-the-index), and the
editor extension is in [In your editor](https://docs.carrick.tools/editor). Without a terminal — CI, an
agent's shell, a pipe — the same lines are written as plain text, with no colour
and no spinner.

`init` runs in this order, and nothing is written — on this machine or in
Carrick — until you accept what it proposes:

1. **The repos this install covers.** In a folder of sibling repos it asks
   which ones, with every repo chosen to begin with. A folder routinely holds a
   repo that should not be indexed, and a repo you leave out gets no proposal
   entry, no project assignment and no connection. Without a terminal, name
   them: `--repo owner/repo`, repeated or comma-separated. A folder of repos
   with neither a terminal nor `--repo` stops there, having written nothing;
   add `--yes` to cover all of them. A single repository is not a choice and is
   never asked about.
2. **The project.** Which project the selected repos belong in.
3. **The proposal**, which names the packages found, the repos covered, any
   project to create, and any repo that would move out of the project it is in
   now. Answering no, or ending any question with Ctrl-C, ends the run with
   nothing written.

Existing users can name the project on the command line:

```
carrick init --project payments --allow-move
```

`--project` puts the selected repos in that project, creating it from the
terminal where the API allows that and printing the project link where it does
not. A repo that is already in another project is **moved** out of it, which
changes what every agent querying either project can see, so the move is named
in the proposal with the project it comes out of and asked about separately.
`--yes` does not grant it: `--allow-move` does, and without either the run
stops before anything is moved.

Connected repos are placed from the terminal, so the GitHub App grant is the
only browser step; where the API has no assignment action, the command prints
the repo-assignment link and, in an interactive terminal, opens the page.
Either way the claim comes from a read. The CLI reads `resolve-repos` until
every covered repo reports `project_slug: "payments"`, and a repo connected to
another project remains pending. A target it cannot meet does not stop the run.
The hooks, the MCP connection and the proposal are written, and the command
says which browser steps are left.

Without `--project` the project step still runs: the command takes the project
the repos are already in, and otherwise lists the workspace's projects for you
to name one. An empty answer leaves the step to the browser. Every project is
printed as the dashboard shows it, display name and slug both — `Payments
(payments)`.

Each repository is identified by its `origin` remote. A remote written through
a per-account SSH host alias (`git@github.com-work:owner/repo.git`) is resolved
with `ssh -G`, so an alias whose `HostName` is `github.com` is an ordinary
GitHub repository here. When a repository still names none, `init` says which
one it was and what it read, and leaves that repository out of the project and
connection steps rather than dropping it quietly. `carrick init --repo
owner/repo` names the repository in that case: a `--repo` value that matches no
repo in the folder attaches to the one repo there that has no identity.

Credentials live in `$XDG_CONFIG_HOME/carrick/credentials.json`, falling back
to `~/.config/carrick/` on macOS and Linux or `%APPDATA%\carrick\` on Windows.
The file is written with mode 0600. `carrick logout` revokes that file's key
on the server, then removes the file; other machines and editor connections
stay signed in. If Carrick cannot be reached, the file is still removed and
the command says the key is still live. Unset `CARRICK_TOKEN` separately if
you set that override. Every issued key is listed, and can be revoked, at
[your account](https://app.carrick.tools/account).

Rust derives the same services for CI, local indexing and `init`. An existing
`carrick.json` is authoritative. When it is missing, `init` presents workspace
packages and their compiler configuration and writes that proposal to
`.carrick/proposal.json`, which is ignored. Each member in it also carries what
its own manifest says about it. That is `private`, `bin`, `main`, `exports`,
any deployment descriptor in its directory, and the members that depend on it,
so the application-versus-library decision is read rather than re-derived. It
writes no `carrick.json`: your agent turns the proposal into one, so the config
you commit is one somebody has read, and the first scan runs against it.
`carrick index` is the scan that asks Carrick to classify what the
deterministic passes could not, and it refuses to run until a `carrick.json`
exists. It is the only scan a first run makes.

Workspace detection handles a repo root or immediate sibling repositories.
An optional `carrick-workspace.json` adds paths through `repos` and removes
directory names through `exclude`. Where you leave a repo out of the selection,
init writes that name into `exclude` and records it under a `carrick` key
beside it: every later command reads the same answer, so the scans, the editor
hooks and the next `init` all leave that repo alone, and `carrick remove` takes
back the names init added and nothing you wrote yourself. `--repo` naming an
excluded repo is refused and says which file excludes it. `carrick derive
--workspace . --json` previews the Rust proposal without writing files or
scanning.

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
| `carrick init [--repo OWNER/REPO]... [--project SLUG] [--allow-move]` | The repos this install covers, the project, the service proposal in `.carrick/`, the agent hooks, the MCP connection, and the prompt that writes `carrick.json` |
| `carrick doctor` | Re-check that setup: the declared paths, the CI workflow against the current template, the hooks, the MCP connection and how far the index is behind |
| `carrick remove [--keep-login]` | Undo all of that on this machine, and list the files the scaffold added to the repository |
| `carrick index` | Derive the workspace, apply optional repo overrides and write `.carrick/`, with Carrick classifying what the deterministic passes could not |
| `carrick refresh [--service X]` | Re-scan one repo, or all of them, and re-join the index. What the session-start hook runs |
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
connects Claude Code through `claude mcp add`. Cursor, Windsurf and VS Code
keep their configuration outside the workspace, so each is asked about by the
path of its file: the terminal offers the ones whose configuration directory
exists, ticked where the editor itself is detected, and a run with no terminal
writes one only when `--mcp EDITOR` names it. `--yes` does not cover them. A
`carrick` server is added to the file and every other entry in it is left
alone.
An entry init writes carries an `X-Carrick-Install-Id` header: a UUID generated
once, kept in `~/.carrick/install-id`, and sent with every MCP call so a slow
session can be told apart from a busy one. It says nothing about the machine or
the person — `carrick remove` deletes it, and the next `carrick init` mints
another. A `carrick` entry that is already there is left exactly as it is, with
or without the header: the header is part of what a client keys its sign-in on,
and nothing a user sees depends on it, so neither `init` nor `doctor` mentions
an entry that has none.
A machine with no client on it is given the line to run, with the
id already in it:

```
claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp --header "X-Carrick-Install-Id: <id>"
```

The first scan on a repository's default branch writes its hosted index. A
Claude Code session opened in a workspace still waiting for one starts
`carrick refresh` in the background, at most once an hour
(`CARRICK_REFRESH_COOLDOWN_MS` states another gap), so the hosted rows arrive
without a command to remember.

## Checking the setup

```
carrick doctor
```

What Carrick answers depends on decisions made once, at setup: which
directories are services, which shared roots they include, which tsconfig
resolves their types. The index itself is rebuilt by CI on every push to the
default branch; the configuration is not rebuilt by anything. `carrick doctor`
re-reads it and exits non-zero when it finds something:

- every `directory`, `include` and `tsconfig` a `carrick.json` declares exists,
- `.github/workflows/carrick.yml` still holds the current template, printed as
  a diff of the lines it is missing (comments are not compared, and steps of
  your own are shown but are not a finding),
- the agent hook entries are the ones this version installs, and the command
  they run still resolves to this package,
- each agent client on this machine has the Carrick MCP server,
- the hosted index answers for every service, with the local index's distance
  from the working tree and from the default branch as notes.

It writes nothing, runs no scan and costs nothing.

## Removing Carrick

```
carrick remove
npm uninstall -g carrick
```

`carrick remove` reverses `carrick init` on this machine, one line per thing it
removed: the Carrick hook entries in this folder's `.claude` settings, the
hooks and skills it copied into each repo here with their `.git/info/exclude`
lines, the `carrick` MCP server in each agent client's configuration, the `.carrick`
directory, and the saved credential. Other hooks, other MCP servers and the
settings files themselves stay; an MCP server called `carrick` that points at
anything other than `api.carrick.tools` is left alone and reported. Pass
`--keep-login` to keep the credential, and `--workspace DIR` to name a folder
other than this one. Running it twice is safe: the second run says there is
nothing left to remove.

Files the onboarding pull request added to the repository are version
controlled, so the command lists them with the `git rm` line that removes them
rather than deleting them itself, and names the sections — the `## Carrick`
section of an `AGENTS.md`, the hook-pack entries in a committed
`.claude/settings.json`, the `.claude` negations in `.gitignore` — that only
their owner can unpick. Revoking the key itself is a separate action, at
[app.carrick.tools/account](https://app.carrick.tools/account).

## Licence

Elastic License 2.0. See
[LICENSE.md](https://github.com/carrick-tools/carrick/blob/main/LICENSE.md).
