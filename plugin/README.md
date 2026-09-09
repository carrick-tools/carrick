# Carrick plugin: the Claude Code and VS Code manifests

Two host manifests over one package. The hook, the language server and the
scanner are all `carrick` (`npm/carrick`, published to npm); the files here say
which command each host should run and hold no code of their own.

They deliver what the workspace index knows about the file an agent just edited,
without anyone asking for it: the routes and calls in that file, who is on the
other side of them in the other repos on disk, and any contract that no longer
holds. Two delivery channels, one source of facts, no model on the laptop.

Everything here reads two commands: `carrick check <file> --json` for the file
an agent just touched, and `carrick status --json` for the workspace a session
just opened. Both shapes are pinned in
[`docs/local-mode-output.md`](../docs/local-mode-output.md), with
[`carrick-check-0.json`](../docs/schemas/carrick-check-0.json) and
[`carrick-status-0.json`](../docs/schemas/carrick-status-0.json) beside it.
Nothing in this directory computes a verdict, and nothing writes.

The boundary is the CLI's own text when the payload carries `boundary_lines`,
printed as it arrives so the hook, the diagnostic and `carrick check` in a
terminal read alike. Without that field the counts in `boundary` are rendered
here instead, using the wording of `ServiceBoundary::lines`.

## What it needs

- The `carrick` CLI on PATH: `npm install -g carrick`, or `carrick init` in the
  folder that holds your repos, which installs the rest of this for you.
- Node 24 or newer. The CLI checks it first and says so.
- An indexed workspace: `carrick index --workspace <dir>` writes `<dir>/.carrick/`.
  Without it every command answers `not_indexed`, the edit hook stays quiet, and
  the session line says the index is missing and names the command that builds
  one.

## The two channels

| Channel | Where it lands | When it arrives |
|---|---|---|
| PostToolUse hook | the tool result of the Edit, Write or MultiEdit | in the same turn as the edit |
| LSP diagnostics | the Problems panel, and the agent's diagnostics attachment | on the next turn in Claude Code, immediately in an editor |

**One channel per install.** The hook and the server carry the same verdicts, so
an install that runs both would say everything twice and make a measurement of
either channel a measurement of the pair. The Claude Code plugin registers the
hook and the server together and passes `--hooks-installed` to the server, which
then publishes nothing. An editor starts the server without that flag and has no
hook, so the server publishes. `CARRICK_CHANNEL=hook|lsp|off` overrides the
decision and is how a measurement run pins one arm.

`npm run selftest` in `npm/carrick` counts hook contexts and diagnostic
attachments separately for every install shape and fails when the count is
wrong.

## Claude Code

```
claude --plugin-dir /path/to/carrick/plugin
```

That registers both the hooks (`hooks/hooks.json`, which run `carrick hook
post-edit` and `carrick hook session-start`) and the language server
(`.lsp.json`, which runs `carrick lsp --stdio`) in one step. The hook is the channel that delivers; the server is
there for the case below.

Claude Code runs one language server per file extension. With a TypeScript
server already enabled, Carrick's is not started, and the hook covers the
session on its own. Diagnostics also arrive one model turn later than the hook,
and Claude Code drops `relatedInformation`, which is why every counterpart site
is written into the message text as well.

SessionStart runs `carrick status --json` and prints one line per service: what
the index holds for it, the commit it was taken at, how far that repo has moved
since, and up to five of the files that moved. Services of one repo share a
commit and a changed-file count, so the count is stated on the first of them and
the others point at it. Each service's boundary lines follow, as the CLI printed
them. `status` carries no verdict at all, so the line cannot be confused with
either verdict channel and is printed whichever one is delivering.

## VS Code

`vscode/` holds a thin extension: a `LanguageClient` on `carrick lsp --stdio`,
activated on TypeScript files, with no UI of its own. Build and package it with
`npm install && npm run build && npx --yes @vscode/vsce package` in that
directory. `release.yml` packages it at the release version, attaches the
`.vsix` to the GitHub release and publishes it to Open VSX, which is the gallery
Cursor, Windsurf and VSCodium resolve from. The Marketplace publish waits on a
token; until there is one the attached `.vsix` is uploaded by hand.

An editor accepts any number of diagnostic providers per file, so this server
sits beside TypeScript's rather than replacing it, and `relatedInformation`
renders as clickable consumer locations.

## Any other LSP client

The server is client-agnostic. Start it over stdio:

```
carrick lsp --stdio
```

Neovim (`vim.lsp.start`), Helix (`languages.toml`), Zed and JetBrains' LSP
support all take a command and arguments; that is the whole configuration. The
server reads the workspace folder the client sends, falls back to the nearest
`.carrick/` above the file, logs when the two differ, and handles `didOpen`,
`didChange` (debounced) and `didSave`. A pull-only client gets the same answer
from `textDocument/diagnostic`.

## Terminal agents with neither

Codex CLI, aider and anything else without hooks or an LSP client get the CLI
(`carrick check <file>`) and the pull request check. They read the same facts on
demand; nothing pushes into their context.

## Settings, one per surface

Every surface can be turned off on its own, and turning one off leaves the
others exactly as they were. In VS Code they are settings; in any other client
they are `initializationOptions`, under the same names without the `carrick.`
prefix, and a `workspace/didChangeConfiguration` carrying
`{ "settings": { "carrick": { ... } } }` changes one mid-session. A surface
turned off loses its rows on the next publish, not at the next restart.

| Setting | Default | What it turns off |
|---|---|---|
| `carrick.diagnostics` | on | The verdicts in the Problems panel, at their own site and at each counterpart |
| `carrick.boundary` | on | The boundary: the status bar item in VS Code, the file-level row in a client without one |
| `carrick.binary` | the `carrick` on PATH | Not a surface: which CLI to run |

`CARRICK_CHANNEL=off` stays the blunt instrument and silences delivery
entirely.

**The boundary is never dropped, and after carrick#879 it is not always in the
Problems panel.** It says what a scan could not classify, which is what makes an
empty answer readable, so the agent's channel always carries it. For a person it
is one sentence about a service rather than a finding about a file, so a client
that says it has somewhere workspace-shaped to put it — VS Code does, with
`boundarySurface` in its `initializationOptions`, and renders it as a status bar
item with the lines as its tooltip — gets it there instead of on every
TypeScript file it opens. A client that says nothing keeps the file-level
Information row, and `carrick.boundary` turns that off.

## The noise budget

What the Problems panel is allowed to hold, per check:

- Ten findings per file, problems first and then by line, with an eleventh row
  naming the remainder and the `carrick check` that prints it. Rows mirrored
  from another file's finding count against the receiving file's ten.
- Thirty rows across the whole check, with one more row naming what it did not
  show.
- One row per counterpart file for one finding, whatever the number of sites in
  it; the other lines are named in that row and every site is still a clickable
  location.
- Error only where a fact row's verdict claims something, is not compatible, and
  the other side is on this disk. Warning for candidates, for rows that claim
  nothing, and for a routing finding whose counterpart this machine cannot open.
  Hint is never used, because it renders as a faint underline to a human and as
  nothing at all to an agent.

## Environment

| Variable | Default | What it does |
|---|---|---|
| `CARRICK_BIN` | the binary in this install | The scanner to run |
| `CARRICK_NATIVE_BINARY` | the platform package's | Scanner binary the CLI resolves to |
| `CARRICK_CHANNEL` | decided by the install | `hook`, `lsp`, or `off` |
| `CARRICK_TIMEOUT_MS` | `5000` | Time limit for one CLI call |
| `CARRICK_LOG` | unset | File to append the server and hook log to |
| `CARRICK_LOG_QUIET` | unset | `1` keeps the log off stderr |

## What it will not do

- Write anything, anywhere, including to `.carrick/`.
- Fail an edit. Every hook exits 0 whatever the CLI does.
- Fire on a deletion. `rm` and `git mv` through Bash are not tool calls the hook
  matches, so a removed producer surfaces at the next session start or the next
  `carrick check` (E17).
- Block a merge. Local mode is advisory; the pull request check is the gate.

## Working on it

The code these manifests point at is in [`npm/carrick`](../npm/carrick):

```
cd ../npm/carrick
npm install
npm test        # tsc --noEmit and the Node test runner
npm run selftest
```

Tests run against fixture payloads under `npm/carrick/test/fixtures/` and a fake
CLI (`npm/carrick/test/fake-carrick.mjs`), so they need no binary and no index.
`.github/workflows/plugin.yml` runs all three commands plus the VS Code build on
every pull request that touches either directory. The manual smokes, which need
a real binary, are in [SMOKE.md](./SMOKE.md), and the full list of what the
delivery surface has to be proven on, platform by platform and editor by editor,
is in [TEST-PLAN.md](./TEST-PLAN.md).
