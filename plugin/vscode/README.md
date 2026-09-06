# Carrick for VS Code

Publishes the workspace index's verdicts about the file you are editing as
diagnostics: the routes and calls in it, who is on the other side of them, and
any contract that no longer holds. Consumer sites arrive as related locations,
so a finding on a producer is one click from the code that reads it.

There is no UI beyond the Problems panel. That is the point: an editor-hosted
agent reads the Problems panel after its own edits, so a diagnostic is a channel
into the agent that costs nobody a prompt.

## What it needs

- Node 24 or newer on PATH (`carrick.nodePath` names another).
- The `carrick` CLI (`carrick.binary` names another) and a workspace that has
  been indexed with `carrick index`. Without an index the server publishes
  nothing and says so in the Carrick output channel.

## Settings

| Setting | Default | What it does |
|---|---|---|
| `carrick.serverPath` | the copy shipped with the extension | Entry point of the language server |
| `carrick.nodePath` | `node` | Node used to run the server |
| `carrick.binary` | `carrick` on PATH | The CLI the server reads from |

## Building it

```
npm install
npm run build          # compiles src/extension.ts to out/
node bundle-server.mjs # copies the shared server into server/
npx --yes @vscode/vsce package
```

Packaging was run once on 2026-09-06 and produced a 479 KB `.vsix`; the file is
gitignored. Not published to the Marketplace or Open VSX yet. The publisher account and the
Open VSX namespace are owner actions in carrick#710, and the same ticket makes
the server a subcommand of the `carrick` package, at which point this extension
holds a manifest and nothing else.
