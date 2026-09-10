# Carrick for VS Code

**Carrick** puts the workspace index's verdicts inside your editor. It tracks cross-service routes and API calls, flags contracts that no longer hold, and gives one-click navigation between producers and consumers across repositories.

---

## Features

* **Diagnostics in the Problems panel.** Contract mismatches and route issues are published straight to the Problems panel. An editor-hosted agent (Cursor, Windsurf) reads that panel after its own edits, so a diagnostic reaches the agent without anyone writing a prompt.
* **Cross-repo code lens.** A small count and contract status sits above each indexed route and call site. Clicking it lists the matching locations and jumps to one, across repositories. A row the index knows nothing about gets no lens: on a laptop index a missing consumer means "not indexed here", not "nobody calls this", so nothing ever says zero.
* **Cross-repo definition and references.** *Go to Definition* on an API call jumps to the route handler that serves it in the other repo. On a route it lists every call site that reaches it.
* **Boundary in the status bar.** Shows the indexed service and the commit the index was taken at. Hovering shows what that scan could not classify, so an empty Problems panel is readable.
* **Editor compatibility.** Works in VS Code, Cursor, Windsurf and VSCodium. VS Code resolves it from the Visual Studio Marketplace; the others from Open VSX.

---

## Installation and setup

The extension does not bundle a language server. It starts `carrick lsp --stdio` from your environment, so the CLI has to be installed first.

### 1. Install the CLI

Node.js 24 or newer, then:

```bash
npm install -g carrick
```

### 2. Initialise your workspace

In the folder that holds your repositories:

```bash
# Finds the repos, writes the workspace file, runs the first index
carrick init

# Refreshes the index as the code changes
carrick index
```

> **Note:** If the CLI is installed somewhere that is not on `PATH`, point the `carrick.binary` setting at it. Without a usable binary or an index, the server publishes nothing and says why in the **Carrick** output channel.

---

## Extension settings

Each setting turns off its own surface and leaves the others standing. Changes take effect on the next publish, with no window reload.

| Setting | Default | Description |
| --- | --- | --- |
| `carrick.binary` | `""` (the `carrick` on `PATH`) | Path to the Carrick CLI the extension starts `lsp --stdio` on. |
| `carrick.diagnostics` | `true` | Publish the workspace verdicts in the Problems panel. |
| `carrick.definition` | `true` | Cross-repository *Go to Definition* between routes and their callers. |
| `carrick.boundary` | `true` | The status bar item (service, commit) and its tooltip. |
| `carrick.codeLens` | `true` | The lens above a route or call the index holds a counterpart or a mismatch for. |

The full table, including what a non-VS-Code client sends instead and the cap on how many rows one check may publish, is in [the plugin README](../README.md).

---

## Development and building

The extension and the scanner are always the same version, because the CLI is the only server there is.

```bash
# Install dependencies
npm install

# Compile src/extension.ts to out/
npm run build

# Package the extension as a .vsix
npx --yes @vscode/vsce package
```

The generated `.vsix` is gitignored. `release.yml` runs the same `vsce package` at the release version, attaches the `.vsix` to the GitHub release and publishes it to Open VSX.
