# Test plan: the delivery surface

What shipped this week is a way of getting Carrick onto a machine and into the
place a developer or an agent is working: one npm package (`carrick`), a Claude
Code plugin, and a VS Code extension, all three over the same CLI and the same
language server. This plan is how that surface gets proven.

[SMOKE.md](./SMOKE.md) is the record of what has been run and what it showed. This
is the list of what must be run, once per platform and once per host, with the
command for each and the observation that decides it. Anything a machine can
decide is named here as a job or a script rather than a box to tick, and where
that job does not exist yet it carries a ticket number, so the plan is executable
and the gap is visible.

Refs carrick#709, carrick#710.

## How to read a row

Every row carries one of three statuses.

| Status | Means |
|---|---|
| **CI** | A job already decides it on every relevant pull request. Nobody runs it by hand; if it is red, read the job. |
| **Manual** | Nothing decides it today. The command is here, and it can be run now. Where a job should exist, the ticket is named. |
| **Blocked** | Cannot be run at all until an owner action lands. Named with what it waits on, and with the nearest offline stand-in where one exists. |

A row with no observation is not a test. Every row below says what to look at
and what it should say.

## 0. The fixture, and the build under test

Two workspaces are used, and they answer different questions.

- **`tests/fixtures/local-mode-workspace`** (in this repo, two services): anyone
  can build it, so it is what CI and a fresh clone use. It carries a real
  producer and consumer pair, and its `loader` to `action` edit is the contract
  break the root-guard smoke uses.
- **The demo workspace** (`test_repos/demo-services` on the owner's machine,
  three services: `user-service`, `order-service`, `notification-service`): the
  workspace every recorded result so far was measured on. Its known finding is
  `GET /api/users`, served by `user-service`, called by `notification-service`
  at `server.ts:25`, and it is a real `type_mismatch`. Use it wherever a row
  says "the demo workspace", so results across rows compare.

Both need an index before anything else answers, and the index has to be built
by the same build that is under test, because the index records the scanner
version that wrote it.

**The build under test.** Every install row starts from a packed tarball rather
than the checkout, because two of the three defects packaging has produced so far
were invisible to the checkout and obvious to an install (carrick#832, #833).

```
cd <carrick checkout>
cargo build --release
(cd src/sidecar && npm ci && npm run build)
(cd npm/carrick && npm ci && npm pack --pack-destination /tmp/pack)
node npm/platform/build.mjs \
  --platform "$(node -p process.platform)" --arch "$(node -p process.arch)" \
  --version "$(node -p "require('./npm/carrick/package.json').version")" \
  --binary target/release/carrick --out /tmp/plat
(cd /tmp/plat/* && npm pack --pack-destination /tmp/pack)
```

Expect two tarballs in `/tmp/pack`: `carrick-<version>.tgz` and
`carrick-tools-cli-<platform>-<arch>-<version>.tgz`. `npm pack` runs `prepack`,
which builds the TypeScript emit, bundles the sidecar and fails on an undeclared
import, so a failure here is a real one and not a packaging accident.

Record the version once, at the top of the results table. Everything else in the
plan is about that build.

## 1. Package install matrix

### What CI already decides

`.github/workflows/plugin.yml` runs on every pull request touching `npm/**`,
`plugin/**` or `src/sidecar/**`, and decides these:

- the package's own tests and typecheck (`npm test`), and the one-channel rule
  (`npm run selftest`);
- a pack, then `npm install --ignore-scripts` of the tarball into a bare
  directory, then `carrick --version` from `node_modules/.bin`;
- the message a user with no platform package gets: it must contain
  `is not installed` and must not be a stack trace;
- a real type verdict from the checkout sidecar **and** from the installed one,
  through `npm/carrick/scripts/verdict-probe.mjs`. This is the row that matters
  most and the one nothing else can see: a sidecar that cannot find its vendored
  `pnpm` degrades every pair to `unverifiable`, which reads exactly like the two
  types agreeing (carrick#833);
- the VS Code extension compiles (`plugin/vscode`, `npm ci && npm run build`).

`.github/workflows/release-build.yml` decides, on any pull request touching
`release.yml`, `src/sidecar/**`, `Cargo.*` or `npm/**`, that the Action's release
tarball still builds, holds `carrick`, `sidecar/package.json`,
`sidecar/package-lock.json` and `sidecar/dist/src/index.js`, and that the binary
inside it runs.

Since carrick#853 the `install` matrix in `plugin.yml` packs that tarball once and
installs it again on macOS arm64, on Linux arm64, on Windows, and with pnpm and
yarn, with the Node floor, a global install and an upgrade as their own jobs. What
is still by hand is everything that needs the BINARY, because CI installs with
`--omit=optional` and so has no platform package to run.

### The matrix

| # | Row | Status | Command | Expected observation |
|---|---|---|---|---|
| 1.1 | npm, Linux x64, project-local, `--ignore-scripts` | **CI** | `plugin.yml`, steps "Install the packed package as a user would" and the two verdict-probe steps | `carrick --version` prints the version; both probes print `isolation: pnpm` and `compatible` / `incompatible` |
| 1.2 | npm, macOS arm64 | **CI** (install half) | `plugin.yml`, the `install` matrix leg `npm, macOS arm64` | the tarball installs, `carrick --version` prints the version, a missing platform package is a message, and `verdict-probe.mjs` answers against the installed sidecar. The full `carrick index` of the demo workspace stays manual: CI installs with `--omit=optional`, so no platform binary resolves there |
| 1.3 | npm, macOS x64 | **Retired as a row** | n/a | there is no Intel leg and there will not be one: the darwin-x64 platform package is cross-built on the Apple-silicon runner (`release.yml`), the Intel runner labels are being retired, and an install probe resolves no platform binary at all, so the leg would answer 1.2's question again. What is untested is the x64 BINARY, which needs an Intel machine and belongs to whoever has one |
| 1.4 | npm, Linux arm64 | **CI** (install half) | `plugin.yml`, the `install` matrix leg `npm, Linux arm64` on `ubuntu-24.04-arm` | as 1.2. This is the runner the release matrix builds on, and until carrick#853 it had never run |
| 1.5 | npm, Windows x64 | **CI, expected red** (carrick#842) | `plugin.yml`, the `install` matrix leg `npm, Windows x64` | the install, `carrick --version` and the missing-platform message all work; `verdict-probe.mjs` fails with `spawn ...\\node_modules\\.bin\\pnpm ENOENT`, which is the reproduction #842 was missing. The step expects that failure and goes red only if the probe starts passing, which is the day the leg should run the same step as every other one |
| 1.6 | pnpm install | **CI** | `plugin.yml`, the `install` matrix leg `pnpm, Linux x64` | measured 2026-09-09, first run: `isolation: pnpm`, `install_ok: true`, `ts_version: 5.9.3`, and both verdict buckets. The walk does reach the bin directory under pnpm's layout — that was reasoning before this leg and is a measurement now |
| 1.7 | yarn (classic) install | **CI** | `plugin.yml`, the `install` matrix leg `yarn, Linux x64` | as 1.6, measured in the same run |
| 1.8 | yarn Berry with PnP | **Manual** (carrick#843) | the same, in a PnP project | there is no `node_modules` at all, so the walk cannot work by construction. What it should do instead is undecided; record what it actually does, which is the input that ticket needs |
| 1.9 | Global install | **CI** | `plugin.yml`, the `global-install` job | `which carrick` resolves and `carrick --version` answers off PATH. This is the install the README tells people to do, and the one that makes `carrick init` write the short hook command rather than an absolute path |
| 1.10 | npx one-shot | **Blocked on publish** (carrick#834) | `npx --yes carrick@<version> init` | Nothing offline stands in for this: npx of a local tarball cannot resolve the optional platform dependency, so the row is untestable until the package publishes. When it does, the observation is that `init` runs to the end and says it wrote an absolute hook command (carrick#837, fixed in #849) |
| 1.11 | Upgrade from a previous version | **CI** (version half) | `plugin.yml`, the `upgrade` job: install the published `carrick`, then the packed tarball over it | `carrick --version` reports the version at HEAD and a missing platform package is still a message, so no file of the previous install is left answering. The `carrick status --json` half stays manual: it runs the binary, which an `--omit=optional` install does not have |
| 1.12 | Node floor | **CI** | `plugin.yml`, the `node-floor` job: install on 24, then run `bin/carrick.mjs` on 22 | `carrick needs Node 24 or newer; this is Node 22.x` and exit 1. It must be that message and not a syntax error: the floor is checked before any TypeScript is imported, and the whole point is that an old Node gets an answer to "which Node do I need" |
| 1.13 | `--ignore-scripts` | **CI** | as 1.1 | no lifecycle script exists anywhere in the package or the platform packages, so the install works with scripts disabled. If this ever needs one, this row is the reason it cannot have one |
| 1.14 | Marketplace and Open VSX publish | **Blocked on publish** (carrick#834, carrick#710) | n/a | the publisher account and the namespace are owner actions. Until they exist, section 4 installs the `.vsix` by file |

**1.A, the install a row means.** Every "npm install" row above is this, with the
tarballs from section 0 copied to the machine under test:

```
mkdir -p /tmp/probe && cd /tmp/probe
echo '{"name":"probe","private":true}' > package.json
npm install --ignore-scripts /tmp/pack/carrick-tools-cli-*.tgz /tmp/pack/carrick-<v>.tgz
export PATH="/tmp/probe/node_modules/.bin:$PATH"
carrick --version
carrick index --workspace <the demo workspace>
node <checkout>/npm/carrick/scripts/verdict-probe.mjs \
  --sidecar /tmp/probe/node_modules/carrick/sidecar
```

Expected, in order: the version; an index that names each service with its route
and call counts and its boundary lines; and

```
{"isolation":"pnpm","install_ok":true,"ts_version":"5.9.3",
 "buckets":{"agree":"compatible","differ":"incompatible"}}
```

An `isolation` of `unavailable` or a bucket of `unverifiable` is the #833 failure
on a new platform, whatever else the run printed.

## 2. The CLI

Run every row from the workspace root (the directory holding
`carrick-workspace.json` and `.carrick/`) unless the row says otherwise. All of
this is **Manual**: the package's own suite drives a fake binary, so nothing in
CI runs these commands against a real index.

### The happy paths

| # | Command | Expected observation | Exit |
|---|---|---|---|
| 2.1 | `carrick init` | asks nothing until it has an identity, then lists the repos it found, settles the project (the one they are already in, or one picked or created from the list), writes `.carrick/proposal.json` and `.claude/settings.json` and nothing else in the tree, runs no scan, and ends on the prompt that has an agent write `carrick.json` (carrick#987). Re-running adds no second hook entry | 0 |
| 2.1b | `carrick init` with `carrick` off PATH (run the entry point by absolute path, with a PATH holding node, git and gh only) | the hook command it writes is this install's own entry point, and it says so, with `npm install -g carrick` as the way back to the short command. It also says the plugin's language server is started as a bare `carrick`, which that machine cannot resolve, so the hooks are the channel there (carrick#837, #849) | 0 |
| 2.2 | `carrick index` | `indexed N repo(s) in X.Xs at <time>`, one line per service with route and call counts and a short commit, the counterpart link count, then each service's boundary lines. On the demo workspace this takes about 7 s for three repos (7.1 s on 0.3.48, 6.6 s on 0.3.50) | 0 |
| 2.3 | `carrick status` | one block per service: what the index holds, the commit, how far the repo has moved since, and the boundary lines | 0 |
| 2.4 | `carrick status --json` | validates against `docs/schemas/carrick-status-0.json`; `scanner_version` is the build under test | 0 |
| 2.5 | `carrick check <a file with a route>` | the routes and calls in that file, who is on the other side, and any verdict. On the demo workspace, checking `user-service`'s users controller names the `GET /api/users` mismatch and `notification-service server.ts:25` | 0 |
| 2.6 | `carrick check <file> --json` | validates against `docs/schemas/carrick-check-0.json`; `repo` + `file` opens the queried file, and `counterparts[].repo` + `.file` opens each counterpart | 0 |
| 2.7 | `carrick touch <file>` | the same file surface **without** verdicts | 0 |
| 2.8 | `carrick refresh --service <name>` | re-scans that service only and re-joins; the map printed afterwards shows a new commit for it and unchanged commits for the others | 0 |
| 2.9 | `carrick hook post-edit` fed a real Edit payload on stdin (the block in SMOKE.md section 0) | exactly one JSON object carrying `additionalContext`, in well under a second (26 ms measured from an installed package). Nothing at all when the index holds no rows and no boundary for that file | 0 |
| 2.10 | `carrick hook session-start < /dev/null` | one line per service, then each service's boundary lines, ending with a newline (carrick#838). The boundary lines must be the same bytes `carrick status` prints: run both into files and diff the boundary block of one against the other | 0 |
| 2.11 | `carrick lsp --stdio` | sits waiting on stdin, and writes `start pid <n> node <version>` to stderr. `CARRICK_LOG=<file>` appends the same lines to that file **as well**; `CARRICK_LOG_QUIET=1` is what keeps them off stderr. Section 4 drives it properly | n/a |
| 2.12 | `carrick templates workflow` | the CI workflow on stdout, with `{{ACTION_REF}}` and `{{DEFAULT_BRANCH}}` filled from the defaults. `carrick templates workflow --action-ref owner/repo@v2` overrides one | 0 |

### The error paths

These are the rows that decide whether a silence is explained. Each says what a
user sees when the thing they did was wrong.

| # | Situation | Command | Expected observation | Exit |
|---|---|---|---|---|
| 2.13 | No GitHub identity | `env -u GITHUB_TOKEN -u GH_TOKEN PATH="$(dirname "$(command -v node)"):/usr/bin:/bin" node <install>/node_modules/carrick/bin/carrick.mjs init` (a PATH with node on it and no `gh`) | `carrick init needs your GitHub identity, and this machine has none it can use.`, then the two ways to give it (`gh auth login`, `GITHUB_TOKEN`). Nothing is written | 1 |
| 2.14 | No index | `carrick check <file>` from a directory with no `.carrick/` above it | `carrick: no local index for this file. Run \`carrick index --workspace <dir>\` in the folder holding your repos.` on stderr | **0** |
| 2.15 | No index, JSON | the same with `--json` | the same stderr line, plus a JSON object on stdout carrying `"error": "not_indexed"` and the schema id. A hook must never fail an edit, which is why this exits 0 | 0 |
| 2.16 | File outside the workspace | `carrick check /etc/hosts` | `not_in_workspace` in the same two forms | 0 |
| 2.17 | Wrong cwd | `cd <workspace>/user-service && carrick check src/users/users.controller.ts` | it still answers. The walk starts at the **file**, not the working directory, and takes the nearest ancestor holding `carrick-workspace.json` or `.carrick/index.json`. A language server is the case this does not cover, because a client sends it a folder rather than a file, which is what the root guard in 3.5 is for | 0 |
| 2.18 | Stale index | edit a producer file, then `carrick check <that file> --json` without re-indexing | `changed_since_index` is non-zero and `stale` is true. The verdict **state does not move**: freshness left the verdict deliberately, so a `resolved` row stays `resolved` and its `detail` gains `(unresolved since your edit: <repo> has changed since it was indexed, so this describes the tree the index was built on)`. Read the state for what the index found and the envelope for whether the tree still matches | 0 |
| 2.19 | Deleted file | delete an indexed file, then check it | `deleted` and `stale` are both true, the index still holds the file's rows, and each verdict becomes `not_checked` / `producer_removed` with `this file is gone and the index still serves <op> here: producer removed, N consumer(s)`. The human render leads with `this file is no longer on disk`. Deletions fire no channel of their own (E17): the next session start or the next explicit check is where this surfaces | 0 |
| 2.20 | No file argument | `carrick check` | `carrick: \`carrick check\` needs a file path` | **2** |
| 2.21 | Unknown option | `carrick check foo.ts --deep` | `carrick: unknown option for \`carrick check\`: --deep` | 2 |
| 2.22 | No workspace file | `carrick index` in an empty directory | `no carrick-workspace.json found here or above`, with an example of one | 1 |
| 2.23 | Workspace names a missing repo | add `"./nope"` to the repos list, `carrick index` | a line naming `./nope` and saying it is not indexed, and the other repos index anyway | 0 |
| 2.24 | Unknown hook | `carrick hook nonsense` | `carrick hook needs one of: post-edit, session-start` | 2 |
| 2.25 | Unknown template | `carrick templates nonsense` | `carrick templates: <what went wrong>` | 2 |
| 2.26 | Platform package missing | `carrick check foo.ts` from an install with no platform tarball | a message containing `is not installed`, not a stack trace (this one is **CI**, in `plugin.yml`) | 1 |
| 2.27 | Corrupt settings file | put invalid JSON in `.claude/settings.json`, run `carrick init` | `skipped .claude/settings.json: it is not valid JSON (...)`, and nothing overwritten | 0 |

Two further things to record while in here, because they are cheap and they have
been wrong before:

- **Read commands write nothing.** Note the size of `~/.carrick/logs/carrick.log.<date>`,
  run twenty `carrick check` calls, note it again. It must be identical: only
  `index` and `refresh` initialise logging (carrick#851).
- **The log is bounded.** `CARRICK_LOG_MAX_MB=1 carrick index` on a workspace big
  enough to exceed it must roll to `carrick.log.<date>.1`, say so on stderr, and
  keep exactly one rolled generation.

## 3. Claude Code

Two channels, and **one of them speaks per install**. The plugin registers the
hook and the language server together and passes `--hooks-installed` to the
server, which then publishes nothing; an editor starts the server without that
flag and has no hook, so the server publishes. `CARRICK_CHANNEL=hook|lsp|off`
overrides, and is how a measurement pins one arm.

There is **no plugin marketplace manifest in this repository** (nothing matching
`marketplace` is tracked), so `--plugin-dir` is the only install path today.
Publishing one is carrick#710 territory; when it lands, every arm below is re-run
through it and the results table gains a column.

Two plugin directories must both be exercised, because one is a copy of the
other made at pack time and a copy can go stale:

- the checkout: `<carrick checkout>/plugin`;
- the install: `<install>/node_modules/carrick/plugin`, which is what
  `carrick init` prints and what a user who never clones this repo will use.

### The trap, before any arm

`--allowedTools` **pre-approves** and does not restrict; it is safe and every arm
should carry the same list. `--disallowedTools` is different: with it present,
Claude Code never calls `getLSPDiagnosticAttachments`, so a server publishing
exactly the right diagnostics delivers nothing (measured 2026-09-09, carrick#709).
Forbidding Bash to force the Edit tool is the natural thing to do and it silently
disables the thing being measured. No arm in this section passes
`--disallowedTools`. The prompt asks for the Edit tool instead, and a run in
which the model edited through Bash is invalid rather than failed (carrick#848).

| # | Row | Status | Command | Expected observation |
|---|---|---|---|---|
| 3.1 | The manifest loads | **Manual** | `claude --debug --debug-file /tmp/cc.log -p 'say hi' --plugin-dir <plugin dir>` then `grep -i lsp /tmp/cc.log` | `Loaded 1 LSP server(s) from plugin: carrick` and a registered notification handler. This is the cheap pre-check: it separates "the manifest is wrong" from "the server did not start", and it costs a `say hi` |
| 3.2 | Hook arm, headless | **Manual** | `CARRICK_CHANNEL=hook claude -p '<task naming the Edit tool>' --allowedTools 'Read,Edit,Write,Bash,Grep,Glob' --plugin-dir <plugin dir>` | in the session transcript, at least one `hook_additional_context` attachment and **zero** `diagnostics` attachments. The model raises the finding unprompted in its answer |
| 3.3 | LSP arm, headless | **Manual** | the same with `CARRICK_CHANNEL=lsp` | the reverse: zero hook contexts, at least one `diagnostics` attachment. The server's log shows `the hook channel owns delivery` only in the hook arm |
| 3.4 | Control arm | **Manual** | the same task with no `--plugin-dir` | neither attachment. Without this arm the other two prove nothing about the channel |
| 3.5 | Root guard | **Manual** | `CARRICK_CHANNEL=lsp CARRICK_LOG=/tmp/lsp.log claude -p 'cd <service>, read <file>, then edit its route response with the Edit tool and stop.' --allowedTools 'Read,Edit,Bash' --plugin-dir <plugin dir>`, then `grep using /tmp/lsp.log` | `client workspace folder <...>/<service> has no .carrick/; using <workspace> (project_dir)` and diagnostics still published. A run rooted at the service directory is a failure even if it printed something |
| 3.6 | The Bash hole | **Manual** | a run whose prompt invites a `sed -i` edit | **nothing on either channel**, and `CARRICK_LOG` never written, because the server is started lazily and only by the Edit or Write tool, and the hook matcher is `Write\|Edit\|MultiEdit`. This row is a reproduction of carrick#848, not a failure of the build; record it so the ticket has a second data point |
| 3.7 | Interactive session | **Manual** | `claude --plugin-dir <plugin dir>` in the workspace, then edit a producer file through the Edit tool | the session-start line appears at the top of the session (one line per service plus boundary lines), and the finding arrives with the edit. Record whether the session line is legible at a glance, since it is the first thing a new user sees |
| 3.8 | Another server owns `.ts` | **Manual** | run 3.3 with a second plugin whose `.lsp.json` claims `.ts` | Claude Code runs one language server per file extension, so Carrick's is not started. The hook must still be the channel and the session must not be silent |
| 3.9 | No index | **Manual** | run 3.2 in a workspace with no `.carrick/` | the hook stays quiet on edits and the session-start line says the index is missing and names the command that builds one. Silence with no explanation is the failure |
| 3.10 | Grading | **Manual** (script: carrick#854 covers the LSP half) | `TRANSCRIPT=$(ls -t ~/.claude/projects/*/*.jsonl \| head -1)`, then count `hook_additional_context` and `diagnostics` attachments with `jq` as SMOKE.md section 1 does | grade from the transcript, never from the printed log: diagnostics are absent from `stream-json`. Also count `Edit\|Write\|MultiEdit` tool calls, and discard a run with zero |

## 4. Editors, over the same language server

The server is client-agnostic and does not know which editor it is talking to. It
takes the workspace folder the client sends, falls back to the nearest `.carrick/`
above the file, logs when the two differ, and handles `didOpen`, `didChange`
(debounced 400 ms), `didSave` and pull-mode `textDocument/diagnostic`.

**Run 4.0 first, in every case.** Most of what an editor shows can be seen
without the editor, by driving `carrick lsp --stdio` the way a client does. Doing
that first separates "the server has nothing to say about this workspace" from
"this editor is not rendering it", which otherwise costs an hour per editor. Today
that is one command (carrick#854):

```
node npm/carrick/scripts/lsp-probe.mjs --workspace $WS \
  --open $WS/<producer>/src/routes/users.ts
```

It prints one row per published URI — the file, the diagnostic count, the
severities, the codes, and whether the `relatedInformation` locations exist on
disk — and exits non-zero when the server never started, published nothing, or
pointed at a file that is not there. The server's own log lines arrive on stderr
as it runs, so the root it chose is in the same output. `--server` points it at
an installed `node_modules/carrick/dist/server.js` instead of this checkout, and
`--json` prints the raw publishes so two runs can be diffed.

**4.0 expected observation** (recorded on the demo workspace, 2026-09-08):

- the producer file gets an **error** diagnostic on the route line, reading
  `GET /api/users type_mismatch: ... Response not assignable to number`, with
  the consumer's site as `relatedInformation`;
- the consumer file, **which the client never opened**, gets its own diagnostic
  naming the producer;
- an **information** diagnostic at line 1 of each checked file carries the
  boundary;
- each check takes tens of milliseconds, and the root the server chose is logged.

Counterpart sites appear in **both** the message text and `relatedInformation`,
deliberately: Claude Code drops the structured field, editors render it as
clickable locations. So in an editor the observation is the clickable location;
in Claude Code it is the site in the text.

### Per editor

| # | Editor | Status | Install path | Activation trigger |
|---|---|---|---|---|
| 4.1 | VS Code, from `.vsix` | **Manual** | `cd plugin/vscode && npm install && npm run build && npx --yes @vscode/vsce package`, then `code --install-extension carrick-<version>.vsix` | opening any `.ts` or `.tsx` file (`onLanguage:typescript`, `onLanguage:typescriptreact`) |
| 4.2 | VS Code, from the Marketplace | **Blocked** (carrick#710) | `code --install-extension carrick-tools.carrick` | as 4.1. Waits on the publisher account |
| 4.3 | Cursor | **Manual** for the `.vsix` (`cursor --install-extension <file>.vsix`), **Blocked** for the gallery | Cursor resolves extensions from Open VSX, not the Marketplace, so the gallery row waits on the Open VSX namespace, not on 4.2 | as 4.1 |
| 4.4 | Windsurf | as 4.3, with `windsurf --install-extension` | same | as 4.1 |
| 4.5 | JetBrains | **Manual** | two candidate paths, and part of this row is finding out which works: **LSP4IJ** (a free plugin, works on the Community editions) configured with a new language server whose command is `carrick` and whose arguments are `lsp --stdio`, mapped to TypeScript files; or the **native LSP API**, which needs a plugin of its own written against it and is paid-IDE only. Record which path was used, and whether the other is viable | opening a TypeScript file after the server is configured |
| 4.6 | Zed | **Manual** | again two routes: whether the installed Zed build accepts a custom server binary through settings, or whether it needs a small Zed extension to register one. Record which the build allows, and if it is the extension, that is a ticket rather than a step | opening a TypeScript file |
| 4.7 | Neovim | **Manual** | there is no `nvim-lspconfig` entry for this server and none is claimed. Start it directly: `vim.lsp.start({ name = 'carrick', cmd = { 'carrick', 'lsp', '--stdio' }, root_dir = vim.fs.root(0, { '.carrick' }) })` from a `FileType` autocommand on `typescript,typescriptreact`. An `nvim-lspconfig` entry is worth filing once the package publishes | the autocommand, on opening a TypeScript buffer |

### What to observe, in every editor

Do these five in order, in each editor, on the demo workspace, and record the
answer for each in the section 6 table.

1. **The server started.** VS Code and its forks: Output panel, "Carrick"
   channel, first line `Starting carrick lsp --stdio. If that command is not on
   PATH ...`. Other editors: their LSP log. If it did not start, go to 5 below
   before anything else.
2. **Producer diagnostic.** Open the producer file. An error appears in Problems
   on the route line with the mismatch text, and the mark spans the statement on
   that line rather than one character of the indent (carrick#922).
3. **Consumer diagnostic.** Open the consumer file. It carries its own
   diagnostic naming the producer. The stronger version of this observation is
   that it appears **without** the consumer being opened at all, since the server
   publishes for counterpart files too; check the Problems panel after opening
   only the producer. With both files open, each keeps every row of its own
   check and the same finding is listed once, not twice (carrick#923).
4. **Related information renders.** The consumer site under the producer's
   diagnostic is a clickable location that opens the right file at the right
   line. This is the field Claude Code drops and the one an editor exists to
   show. A location that opens nothing means the counterpart's `repo` was null or
   the join was wrong, and that is worth a ticket with the payload attached.
5. **The boundary line.** An information diagnostic at line 1 of each checked
   file, carrying the boundary as the CLI prints it. It is never dropped, and on
   a service written the ordinary way it may be the only thing shown, because a
   local index holds deterministic rows only. It is still there after opening
   the file on the other side of the row, which is what carrick#923 was.

### Workspace root, multi-root and monorepos

The server takes **`workspaceFolders[0]`** and nothing else, and it does not
handle `didChangeWorkspaceFolders`. So:

| # | Case | Expected | Status |
|---|---|---|---|
| 4.8 | Open the workspace folder (the directory holding `.carrick/`) | root taken from the client, no note logged | Manual |
| 4.9 | Open one service as the folder | the walk finds `.carrick/` above it and logs `client workspace folder <...>/<service> has no .carrick/; using <workspace> (ancestor)`. Diagnostics still arrive | Manual |
| 4.10 | Multi-root, first folder inside the indexed tree | as 4.9, for every folder | Manual |
| 4.11 | Multi-root, first folder **outside** the indexed tree | the root is resolved from a folder with no `.carrick/` above it, so files in the second folder are expected to get nothing. Record the log line and the Problems panel; this is a finding to file, not a pass or a fail, and the plan is what finds it | Manual |
| 4.12 | A folder added to a running session | not handled: the server is not told. Record whether a reload fixes it | Manual |
| 4.13 | A monorepo of several services in one repo | the workspace root is the folder holding the repos, not the repo, and not a package inside it. Record what the editor sent and what the server chose | Manual |

### The binary, and how each host found it

A GUI application launched from a dock or a launcher does not have a shell's
PATH, so a `carrick` that works in a terminal can be missing in an editor. Only
the VS Code extension has a setting for it (`carrick.binary`); the Claude Code
manifest has no equivalent, and a static manifest cannot resolve a path
(carrick#837 fixed the half `carrick init` owns, not this half). Record, per
editor, one of: PATH, `carrick.binary`, or a wrapper. `CARRICK_LOG` is also not
inherited by a GUI application launched that way, so the Output channel is where
the root line is read.

## 5. Evals

The measurement of whether this surface changes what an agent concludes belongs
in the corpus harness, which lives in carrick-cloud and is the cloud engineer's.
The proposal, in full, is filed as **carrick-cloud#712**: a CLI arm (the binary on
PATH, a fresh local index, no MCP) and a plugin arm (the hook channel, installed
with `--plugin-dir`), both headless, both graded by the existing judge.

Three things from it that belong here as well, because they are facts about this
surface rather than about the harness:

- the harness passes `--disallowedTools` on network-denied cases, and that flag
  stops Claude Code fetching diagnostic attachments, so an LSP-channel arm reads
  as zero delivery until the denial moves into the isolation settings. The hook
  channel is unaffected;
- arm health has to assert the binary answers inside the run's own environment
  and that the index's commit matches the tree, or a run measures a channel that
  was never there;
- a run in which the model edited only through Bash is invalid rather than
  failed (carrick#848).

Nothing in this section is run from here, and no eval is run as part of this plan.

## 6. Results

Copy this table, one row per machine or host, and fill it in. Record the build
under test once at the top: scanner version, package version, and the commit.

**Build under test:** version 0.3.50, tag `v0.3.50` (`46428cc`; the tree it was
packed from differs from that tag in this file and two other markdown files
only), packed 2026-09-09 on macOS 15.5 arm64, Node 24.12.0.

**Fixture:** the three-service demo workspace, re-indexed by the build under
test before section 2 (6.6 s, `user-service` 3 routes 1 call, `order-service`
0 routes 4 calls, `notification-service` 0 routes 3 calls, 6 counterpart links).

### Install matrix

| Row | Platform / manager | Ran on | `--version` | `index` | verdict probe | Notes |
|---|---|---|---|---|---|---|
| 1.2 | npm, macOS arm64 | **CI** (install half) | `plugin.yml`, the `install` matrix leg `npm, macOS arm64` | the tarball installs, `carrick --version` prints the version, a missing platform package is a message, and `verdict-probe.mjs` answers against the installed sidecar. The full `carrick index` of the demo workspace stays manual: CI installs with `--omit=optional`, so no platform binary resolves there |
| 1.3 | npm, macOS x64 | **Retired as a row** | n/a | there is no Intel leg and there will not be one: the darwin-x64 platform package is cross-built on the Apple-silicon runner (`release.yml`), the Intel runner labels are being retired, and an install probe resolves no platform binary at all, so the leg would answer 1.2's question again. What is untested is the x64 BINARY, which needs an Intel machine and belongs to whoever has one |
| 1.4 | npm, Linux arm64 | **CI** (install half) | `plugin.yml`, the `install` matrix leg `npm, Linux arm64` on `ubuntu-24.04-arm` | as 1.2. This is the runner the release matrix builds on, and until carrick#853 it had never run |
| 1.5 | npm, Windows x64 | **CI, expected red** (carrick#842) | `plugin.yml`, the `install` matrix leg `npm, Windows x64` | the install, `carrick --version` and the missing-platform message all work; `verdict-probe.mjs` fails with `spawn ...\\node_modules\\.bin\\pnpm ENOENT`, which is the reproduction #842 was missing. The step expects that failure and goes red only if the probe starts passing, which is the day the leg should run the same step as every other one |
| 1.6 | pnpm install | **CI** | `plugin.yml`, the `install` matrix leg `pnpm, Linux x64` | measured 2026-09-09, first run: `isolation: pnpm`, `install_ok: true`, `ts_version: 5.9.3`, and both verdict buckets. The walk does reach the bin directory under pnpm's layout — that was reasoning before this leg and is a measurement now |
| 1.7 | yarn (classic) install | **CI** | `plugin.yml`, the `install` matrix leg `yarn, Linux x64` | as 1.6, measured in the same run |
| 1.8 | yarn Berry PnP | | | | | |
| 1.9 | Global install | **CI** | `plugin.yml`, the `global-install` job | `which carrick` resolves and `carrick --version` answers off PATH. This is the install the README tells people to do, and the one that makes `carrick init` write the short hook command rather than an absolute path |
| 1.11 | Upgrade from a previous version | **CI** (version half) | `plugin.yml`, the `upgrade` job: install the published `carrick`, then the packed tarball over it | `carrick --version` reports the version at HEAD and a missing platform package is still a message, so no file of the previous install is left answering. The `carrick status --json` half stays manual: it runs the binary, which an `--omit=optional` install does not have |
| 1.12 | Node floor | **CI** | `plugin.yml`, the `node-floor` job: install on 24, then run `bin/carrick.mjs` on 22 | `carrick needs Node 24 or newer; this is Node 22.x` and exit 1. It must be that message and not a syntax error: the floor is checked before any TypeScript is imported, and the whole point is that an old Node gets an answer to "which Node do I need" |

### CLI

Run 2026-09-09 on macOS arm64, from the installed package of the build above,
against the demo workspace unless the row names another. Every row's output was
captured; the notes quote it where the wording is the observation.

| Row | What | Result | What it said |
|---|---|---|---|
| 2.1 | `init` | **pass** | identity `daveymoores (from the GitHub CLI)`, repo list, `wrote carrick-workspace.json`, `wrote .claude/settings.json`, first index, then the MCP line and the workflow line. Second run: `unchanged` for both files, still two hook entries. Run on a scratch two-repo workspace so the fixture keeps no `.claude/` of its own |
| 2.1b | `init` with `carrick` off PATH | **pass** | writes the absolute entry point into the hook command and says so: "`carrick` is not on PATH here, so those hooks name this install", plus how to get the short command back. This is carrick#837 / #849 proven from an install rather than a checkout |
| 2.2 | `index` | **pass** | `indexed 3 repo(s) in 6.6s`, a line per service, `6 counterpart link(s)`, then each service's boundary lines |
| 2.3 | `status` | **pass** | workspace line, three service lines with `changed since index: 0`, then the boundary blocks |
| 2.4 | `status --json` | **pass** | `schema: carrick.status/0`; every required field present at both levels; no key outside the schema; `scanner_version` is the build that wrote the index |
| 2.5 | `check <producer>` | **pass** | the three routes, and on `GET /api/users` the `type_mismatch` with `notification-service server.ts:25` as the consumer. The other two routes are `unresolved`, which is a claim about nothing rather than a claim of agreement |
| 2.6 | `check --json` | **pass** | `schema: carrick.check/0`; `repo` + `file` opens the queried file; all three `counterparts[].repo` + `.file` open a file that exists; four `boundary_lines` and a `boundary_note` |
| 2.7 | `touch` | **pass** | the same surface with `verdict: null` on every item, by contract. The human render simply omits the verdict line |
| 2.8 | `refresh --service` | **pass** | one repo re-scanned in 2.8 s against 6.6 s for all three; the other two commits unchanged; counterpart links preserved |
| 2.9 | `hook post-edit` | **pass** | one `hookSpecificOutput.additionalContext` object, **27 ms** for the check (74 ms wall including Node start), carrying the real mismatch and the consumer. A file with no rows still gets the boundary; a file outside the workspace gets nothing at all |
| 2.10 | `hook session-start` | **pass** | one line per service, boundary lines after, trailing newline present (carrick#838). The 13 boundary lines are **byte-identical** to `carrick status` |
| 2.11 | `lsp --stdio` | **pass** | `start pid <n> node v24.12.0` on stderr, nothing on stdout. `CARRICK_LOG` writes the same line to the file **as well**; `CARRICK_LOG_QUIET=1` is what empties stderr |
| 2.12 | `templates workflow` | **pass** | the workflow on stdout with no `{{` placeholder left |
| 2.13 | no identity | **pass** | `carrick init needs your GitHub identity, and this machine has none it can use.` plus the two ways to give it. Exit 1, nothing written |
| 2.14 | no index | **pass** | `no local index for this file. Run carrick index --workspace <dir> ...`, exit 0 |
| 2.15 | no index, `--json` | **pass** | the same line on stderr, `{"schema":"carrick.check/0","error":"not_indexed"}` on stdout, exit 0 |
| 2.16 | file outside the workspace | **pass** | `this file is not under any repo the workspace lists ...` and `"error":"not_in_workspace"`, exit 0 |
| 2.17 | wrong cwd | **pass** | answers from inside a service directory: the walk starts at the file. Exit 0 |
| 2.18 | stale index | **pass** | `stale: true`, `changed_since_index: 1`, and every verdict's detail gains the "since your edit" sentence while its state stands. The row's original expectation was wrong and is corrected above |
| 2.19 | deleted file | **pass** | `deleted: true`, rows still served, each verdict `not_checked` / `producer_removed` with the consumer count, and the render leads with `this file is no longer on disk` |
| 2.20 | no file argument | **pass** | ``carrick: `carrick check` needs a file path``, exit 2 |
| 2.21 | unknown option | **pass** | ``carrick: unknown option for `carrick check`: --deep``, exit 2 |
| 2.22 | no workspace file | **pass** | `no carrick-workspace.json found here or above`, with an example, exit 1 |
| 2.23 | workspace names a missing repo | **pass** | names `./nope`, says it is not indexed, indexes the other two anyway |
| 2.24 | unknown hook | **pass** | `carrick hook needs one of: post-edit, session-start`, exit 2 |
| 2.25 | unknown template | **pass** | `carrick templates: no carrick template named nonsense`, exit 2 |
| 2.26 | platform package missing | **pass** | names the platform package, offers `CARRICK_NATIVE_BINARY`, exit 1, no stack trace |
| 2.27 | corrupt settings file | **pass** | `skipped .claude/settings.json: it is not valid JSON (...)`, and the file was left exactly as it was |
| extra | read commands write nothing | **pass** | twenty `check` and twenty `status` calls left the debug log at 275,646 bytes, unchanged |
| extra | log is bounded | **pass** | with `CARRICK_LOG_MAX_MB=1` and a file past the cap, the run rolled it to one generation and started a new file. The message names the rolled file with a literal `<date>`, which is carrick#858 |

**Found by this run, both filed:** carrick#857, a file rewritten with identical
bytes reads as stale, so every verdict in it carries a caveat the same payload
denies; and carrick#858, the log-roll message prints a placeholder instead of
the file it wrote. Nothing else in section 2 failed.

### Claude Code

| Row | Arm | Plugin dir (checkout / install) | hook contexts | diagnostic attachments | Edit-tool calls | Verdict |
|---|---|---|---|---|---|---|
| 3.2 | hook | | | | | |
| 3.3 | lsp | | | | | |
| 3.4 | control | | | | | |
| 3.5 | root guard | | | | | |
| 3.6 | Bash edit | | | | | |
| 3.7 | interactive | | | | | |

### Editors

| Row | Editor and version | Install path | How it found the binary | Server started | Producer diagnostic | Consumer diagnostic | Related info clickable | Boundary line | Root chosen | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| 4.1 | VS Code | .vsix | | | | | | | | |
| 4.3 | Cursor | .vsix | | | | | | | | |
| 4.4 | Windsurf | .vsix | | | | | | | | |
| 4.5 | JetBrains | LSP4IJ / native | | | | | | | | |
| 4.6 | Zed | settings / extension | | | | | | | | |
| 4.7 | Neovim | vim.lsp.start | | | | | | | | |

### Multi-root and monorepo

| Row | Case | Editor | Log line the server wrote | Diagnostics arrived | Notes |
|---|---|---|---|---|---|
| 4.9 | service as folder | | | | |
| 4.11 | first folder outside the tree | | | | |
| 4.12 | folder added live | | | | |
| 4.13 | monorepo | | | | |

## 7. Known gaps

Open tickets this plan runs into, so a red row can be recognised as a known one.

| Ticket | What it is | Which rows it touches |
|---|---|---|
| carrick#834 | Nothing publishes the package: there is no `npm publish` step in any workflow, and neither `carrick` nor the platform packages resolve on the registry. The pull request that adds it is a draft and must stay one until the npm organisation exists | 1.10, 1.11 (registry form), 1.14, 4.2, 4.3, 4.4 |
| carrick#710 | The packaging ticket itself: the npm organisation, the Marketplace publisher and the Open VSX namespace are the owner actions it lists | 1.14, 3 (marketplace path), 4.2 to 4.4 |
| carrick#833 | The vendored `pnpm` and `tsc` lookup. Fixed in #841 and proven against a packed tarball; deliberately open until a registry install proves it, because the close criterion is the published package | 1.1 to 1.9 |
| carrick#842 | Windows cannot spawn the vendored bins (`.bin/tsc` is a `.CMD` and the spawn has no shell). Expected red | 1.5 |
| carrick#843 | Half answered: pnpm and yarn classic are measured in CI and work (1.6, 1.7). Yarn PnP has no `node_modules` at all, so the walk cannot work there by construction | 1.8 |
| carrick#845 | The package publishes no type declarations, so a TypeScript consumer of `carrick/templates` cannot type the import | not exercised here; it lands with the same release |
| carrick#848 | An edit made through Bash reaches neither channel, because both are keyed to `Write\|Edit\|MultiEdit` and the server is started lazily by the Edit or Write tool | 3.6, and the validity rule in 3.10 |
| carrick#853 | Shipped. The `install` matrix, `node-floor`, `global-install` and `upgrade` jobs in `plugin.yml` | 1.2, 1.4 to 1.7, 1.9, 1.11, 1.12 |
| carrick#854 | Shipped as `npm/carrick/scripts/lsp-probe.mjs`. Driving it against a real index is still by hand, because CI has no index to point it at | 4.0, and the first step of 4.1 to 4.7 |
| carrick#857 | A file rewritten with identical bytes reads as stale, so every verdict in it gains a caveat that the same payload's `changed_since_index: 0` denies. Found by row 2.18's neighbourhood on 2026-09-09 | 2.17, 2.18, and every editor row, since a save with no change is ordinary there |
| carrick#858 | The log-roll message prints `carrick.log.<date>.1` literally rather than the file it wrote. Cosmetic | the log-bound check at the end of section 2 |

And the design facts that are not tickets, because they are how it works. A row
that hits one of these is behaving correctly:

- **Diagnostics land one model turn late** in Claude Code. A run that ends on the
  edit may carry the finding into the final turn or may not; the hook has no such
  hole, because it attaches to the tool result.
- **One language server per file extension** in Claude Code. With another
  plugin's server owning `.ts`, Carrick's is not started and the hook is the
  channel.
- **`relatedInformation` is dropped** by Claude Code and rendered by editors,
  which is why every counterpart site is in the message text as well.
- **Deletions fire nothing.** `rm` and `git mv` through Bash are not tool calls
  either channel matches, so a removed producer surfaces at the next session
  start or the next explicit `carrick check`.
- **A local index holds deterministic rows only.** A route registered on a typed
  receiver and a call whose URL is assembled at the call site need a model, and
  no model runs on the laptop. The boundary line is what stops that reading as
  "there is no API here", which is why it is never dropped.
- **Nothing here writes, and nothing fails an edit.** Every read command exits 0
  whatever it found.
