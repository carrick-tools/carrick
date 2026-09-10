# carrick

Carrick indexes the TypeScript repos sitting beside each other on your disk and
answers, about the file you are editing right now: which routes and calls are in
it, who is on the other side of them in the other repos, and whether the
contract between them still holds. It answers in your editor's Problems panel
and in your coding agent's context, without either of them asking.

```
npm install -g carrick
carrick login
cd ~/code            # a repo, or the folder that holds your repos
carrick init
```

`carrick login` opens the browser to authorise a Carrick workspace. `init`
requires that credential or a `CARRICK_TOKEN` environment override and verifies
it before writing files. GitHub CLI credentials do not grant Carrick access.

Existing users can initialise the local workspace against a named Carrick
project:

```
carrick init --project payments
```

The command shows the current project assignment for every proposed GitHub
repo and prints the project, GitHub App, and repo-assignment links needed for
the remaining browser steps. In an interactive terminal, it also opens the
workspace's project page. The browser creates projects and changes assignments;
the CLI only reads `resolve-repos` until every proposed repo reports
`project_slug: "payments"`. A repo connected to another project remains
pending. In a noninteractive shell, an unmet target prints the same links and
exits with status 1 without writing the local setup. Omit `--project` to keep
the normal workspace init flow.

Credentials live in `$XDG_CONFIG_HOME/carrick/credentials.json`, falling back
to `~/.config/carrick/` on macOS and Linux or `%APPDATA%\carrick\` on Windows.
The file is written with mode 0600. `carrick logout` removes it; unset
`CARRICK_TOKEN` separately if you set that override. Revoke issued keys at
[your account](https://app.carrick.tools/account).

Rust derives the same services for CI, local indexing and `init`. An existing
`carrick.json` is authoritative. When it is missing, `init` presents workspace
packages and their nearest `tsconfig.json`, then creates the config once.
Review service boundaries and shared source includes before committing it.
Repeated init preserves existing config bytes, including hand edits.

Workspace detection handles a repo root or immediate sibling repositories.
An optional `carrick-workspace.json` adds paths through `repos` and removes
directory names through `exclude`; init preserves this file and does not
create one. `carrick derive --workspace . --json` previews the Rust proposal
without writing files or scanning. Deno manifest discovery does not provide
import-map type configuration or Deno globals; the Deno support guard remains.

## What it installs

One package. The scanner is a Rust binary that arrives as
`@carrick-tools/cli-<platform>-<arch>`, an optional dependency npm resolves by
`os` and `cpu`; the type sidecar, the language server and the hook scripts come
with this package. There is no postinstall step and nothing is downloaded after
the install, so `--ignore-scripts` works.

Node 24 or newer, because the sidecar that resolves your request and response
types runs on it.

## The commands

| Command | What it does |
|---|---|
| `carrick login` | Authorise a Carrick workspace in the browser, or verify `CARRICK_TOKEN` |
| `carrick logout` | Remove the saved local credential |
| `carrick init [--project SLUG]` | The repo list, optional project-assignment verification, the first index, the agent hooks, and the lines it cannot run for you |
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
  client on `carrick lsp --stdio`. Editor-hosted agents read the Problems panel
  after their own edits.
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

Deterministic rows: file-based and descriptor routes, class-controller routes,
imported-member calls, GraphQL schema and document rows, socket and pub/sub
operations, and the types on both sides of them. A route registered on a typed
receiver, and a call whose URL is assembled at the call site, are classified in
the hosted index and are absent here — every answer says how many of those it
counted and did not classify, so a thin answer never reads as an empty one.

The hosted index covers every repo in your organisation on its main branch,
including the ones you do not have checked out, and answers over MCP:

```
claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp
```

## Licence

Elastic License 2.0. See
[LICENSE.md](https://github.com/carrick-tools/carrick/blob/main/LICENSE.md).
