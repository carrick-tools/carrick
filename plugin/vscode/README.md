# Carrick for VS Code

Publishes the workspace index's verdicts about the file you are editing as
diagnostics: the routes and calls in it, who is on the other side of them, and
any contract that no longer holds. Consumer sites arrive as related locations,
so a finding on a producer is one click from the code that reads it.

There is no UI beyond the Problems panel. That is the point: an editor-hosted
agent reads the Problems panel after its own edits, so a diagnostic is a channel
into the agent that costs nobody a prompt.

Cursor, Windsurf and VSCodium install it the same way; they resolve extensions
from Open VSX rather than the Marketplace.

## Install the CLI first

This extension holds no server of its own. It starts `carrick lsp --stdio`, so
without the `carrick` CLI on PATH it starts nothing and says so in the Carrick
output channel.

```
npm install -g carrick     # needs Node 24 or newer
```

Then, in the folder that holds your repos:

```
carrick init      # finds the repos, writes the workspace file, runs the first index
```

`carrick index` refreshes it later. Without an index the server publishes no
diagnostics and says so in the output channel.

If the CLI lives somewhere that is not on PATH, point `carrick.binary` at it.

## Settings

| Setting | Default | What it does |
|---|---|---|
| `carrick.binary` | `carrick` on PATH | The CLI to start `lsp --stdio` on |

The extension and the scanner are always the same version, because there is
only one of them: the CLI.

## Building it

```
npm install
npm run build   # compiles src/extension.ts to out/
npx --yes @vscode/vsce package
```

The `.vsix` is gitignored. `release.yml` runs the same `vsce package` at the
release version, attaches the `.vsix` to the GitHub release and publishes it to
Open VSX.
