# Carrick for VS Code

Publishes the workspace index's verdicts about the file you are editing as
diagnostics: the routes and calls in it, who is on the other side of them, and
any contract that no longer holds. Consumer sites arrive as related locations,
so a finding on a producer is one click from the code that reads it.

There is no UI beyond the Problems panel. That is the point: an editor-hosted
agent reads the Problems panel after its own edits, so a diagnostic is a channel
into the agent that costs nobody a prompt.

## What it needs

- The `carrick` CLI on PATH — `npm install -g carrick`, or `carrick init` in
  the folder that holds your repos. `carrick.binary` names another copy.
- A workspace that has been indexed with `carrick index`. Without an index the
  server publishes nothing and says so in the Carrick output channel.

## Settings

| Setting | Default | What it does |
|---|---|---|
| `carrick.binary` | `carrick` on PATH | The CLI to start `lsp --stdio` on |

The extension holds no server of its own: it starts `carrick lsp --stdio`, so
the server and the scanner are always the same version.

## Building it

```
npm install
npm run build   # compiles src/extension.ts to out/
npx --yes @vscode/vsce package
```

The `.vsix` is gitignored. Not published to the Marketplace or Open VSX yet: the
publisher account and the Open VSX namespace are owner actions in carrick#710.
