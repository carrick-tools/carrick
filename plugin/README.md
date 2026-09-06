# Carrick plugin: a hook and a language server over `carrick check`

Delivers what the workspace index knows about the file an agent just edited,
without anyone asking for it: the routes and calls in that file, who is on the
other side of them in the other repos on disk, and any contract that no longer
holds. Two delivery channels, one source of facts, no model on the laptop.

Everything here reads `carrick check <file> --json` and `carrick touch --json`.
The shape is pinned in [`docs/local-mode-output.md`](../docs/local-mode-output.md)
and [`docs/schemas/carrick-check-0.json`](../docs/schemas/carrick-check-0.json).
Nothing in this directory computes a verdict, and nothing writes.

The boundary is the CLI's own text when the payload carries `boundary_lines`,
printed as it arrives so the hook, the diagnostic and `carrick check` in a
terminal read alike. Without that field the counts in `boundary` are rendered
here instead, using the wording of `ServiceBoundary::lines`.

## What it needs

- Node 24 or newer. The server and the hooks are TypeScript run directly.
- The `carrick` CLI on PATH, or `CARRICK_BIN` pointing at it.
- An indexed workspace: `carrick index --workspace <dir>` writes `<dir>/.carrick/`.
  Without it every command answers `not_indexed`, the hooks stay quiet, and the
  session line says the index is missing.

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

`npm run selftest` counts hook contexts and diagnostic attachments separately
for every install shape and fails when the count is wrong.

## Claude Code

```
claude --plugin-dir /path/to/carrick/plugin
```

That registers both the hooks (`hooks/hooks.json`) and the language server
(`.lsp.json`) in one step. The hook is the channel that delivers; the server is
there for the case below.

Claude Code runs one language server per file extension. With a TypeScript
server already enabled, Carrick's is not started, and the hook covers the
session on its own. Diagnostics also arrive one model turn later than the hook,
and Claude Code drops `relatedInformation`, which is why every counterpart site
is written into the message text as well.

SessionStart prints one line: the service, the commit it was indexed at, how
many files have changed since, and the boundary. `carrick touch` returns every
verdict as null, so that line carries no compatibility finding and is printed
whichever channel is delivering.

## VS Code

`vscode/` holds a thin extension: a `LanguageClient` on the same server,
activated on TypeScript files, with no UI of its own. Build and package it with
`npm install && npm run build && node bundle-server.mjs && npx --yes @vscode/vsce package`
in that directory (proved on 2026-09-06: 330 files, 479 KB). It is not published
to the Marketplace or Open VSX; the publisher accounts are owner actions in
carrick#710.

An editor accepts any number of diagnostic providers per file, so this server
sits beside TypeScript's rather than replacing it, and `relatedInformation`
renders as clickable consumer locations.

## Any other LSP client

The server is client-agnostic. Start it over stdio:

```
node /path/to/carrick/plugin/src/server.ts --stdio
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

## Environment

| Variable | Default | What it does |
|---|---|---|
| `CARRICK_BIN` | `carrick` | The CLI to run |
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

```
npm install
npm test        # tsc --noEmit and the Node test runner
npm run selftest
```

Tests run against fixture payloads under `test/fixtures/` and a fake CLI
(`test/fake-carrick.mjs`), so they need no binary and no index.
`.github/workflows/plugin.yml` runs all three commands plus the VS Code build on
every pull request that touches this directory. The manual smokes, which need a
real binary, are in [SMOKE.md](./SMOKE.md).

carrick#710 turns this directory into two manifests: the plugin and the
extension will point at `carrick lsp --stdio` from the published npm package,
and `src/` moves into it.
