# Smokes for the hook and the LSP shim

The plugin's own suite runs against fixture payloads and a fake binary, so none
of it proves anything against a real index. These are the runs that do. Section
0 and section 3 are free; sections 1 and 2 spend a little on model calls.

This file is the record of what has been run. [TEST-PLAN.md](./TEST-PLAN.md) is
the wider list of what must be run for the whole delivery surface: the npm
package on every platform and package manager it claims, the CLI's error paths,
both Claude Code channels, and each editor over the same language server.

Set once for every command below:

```
export WS=/path/to/a/workspace/with/several/repos
```

`carrick` must be on PATH — `npm install -g carrick`, or the local install of
this checkout:

```
cd npm/carrick && npm run build && npm pack --pack-destination /tmp/pack
node npm/platform/build.mjs --platform "$(node -p process.platform)" \
  --arch "$(node -p process.arch)" --version "$(node -p "require('./npm/carrick/package.json').version")" \
  --binary target/release/carrick --out /tmp/plat
(cd /tmp/plat/* && npm pack --pack-destination /tmp/pack)
mkdir -p /tmp/probe && cd /tmp/probe && echo '{"name":"probe","private":true}' > package.json
npm install --ignore-scripts /tmp/pack/*.tgz
export PATH="/tmp/probe/node_modules/.bin:$PATH"
```

## 0. The binary and the two channels, before anything else (free)

```
cd "$WS"
carrick init                 # or: carrick index --workspace "$WS"
carrick check <a file with a route> --json | python3 -m json.tool | head -40
carrick status --json | python3 -m json.tool | head -40
```

Read the two payloads against `docs/schemas/carrick-check-0.json` and
`docs/schemas/carrick-status-0.json`. Then the hook, which is the whole
PostToolUse path:

```
cd "$WS"
echo '{"tool_name":"Edit","cwd":"'"$WS"'","tool_input":{"file_path":"'"$WS"'/<service>/<file>.ts"}}' \
  | carrick hook post-edit
```

Expect one JSON object with `additionalContext`, or nothing at all when the
index holds no rows and no boundary for that file. `CARRICK_LOG_QUIET=0` puts
the timing on stderr.

The session line has its own check:

```
cd "$WS"
carrick hook session-start < /dev/null
```

Expect one line per service and each service's boundary lines last. Compare it
against `carrick status` with no `--json`: the boundary lines are the same bytes
in both.

**Run on 2026-09-08, three-repo workspace, scanner 0.3.48 (carrick#710).**
`carrick index` 7.1 s; `check` returned a real `type_mismatch` with the consumer
named; the hook answered in 26 ms from an installed package and 28 ms from the
checkout; the session line's boundary bytes matched `carrick status`.

**Run on 2026-09-10, `carrick@0.3.58` installed from the npm registry into a
throwaway probe directory** (`npm install carrick@0.3.58`, no `--pack`, no
local build; the global `carrick` on this machine is 0.3.53 and was left
alone), against a scratch copy of `tests/fixtures/local-mode-workspace` (two
repos, `git init`-ed, `carrick-workspace.json` written by hand: this fixture
ships no workspace file of its own). `carrick --version` printed `0.3.58`
before every command below, confirming PATH pointed at the probe install, not
the global one.

`carrick index --workspace "$WS"` indexed 2 repos in 4.8 s, no `--json`: 2
routes / 0 calls, then 0 routes / 2 calls, 4 counterpart links, and the same
"candidates: not classified locally (no model runs on this machine) ... 0
file(s) sent to the analyzer" boundary line for both services. That line is the
proof no LLM call happened; the earlier "Carrick run starting ...
api_endpoint=https://api.carrick.tools" banner is a startup config print, not a
network call, and every boundary line confirms nothing was sent. `check --json`
on the producer route and `status --json` both validated against
`docs/schemas/carrick-check-0.json` and `docs/schemas/carrick-status-0.json`
(every `required` key present at every level, no key outside `properties`).
The route came back `resolved` / `compatible`, which is this fixture's
clean-match state, not a deviation.

`carrick hook post-edit` on the producer file, run with stdout and stderr
captured to separate files: stdout held exactly one JSON object,
`hookSpecificOutput.additionalContext`, parsed clean with `json.load`; stderr
held the one timing line, `carrick-hook: check ... -> context in 30ms`, with no
env var set. `carrick hook session-start < /dev/null` printed one line per
service plus the boundary lines last; those boundary lines were
**byte-identical** to `carrick status` (no `--json`) run straight after.

Then the fixture's own contract break: `export async function loader` renamed
to `export async function action` in the route module by hand, its documented
method-mismatch edit, plus `carrick refresh --service catalog-web`. `check
--json` on the consumer file now returned `verdict.result:
"method_mismatch"`, `"this call uses GET and the producer serves POST at
/api/v1/widgets/:widgetId"`, matching the fixture's README answer key. The
producer side showed `verdict.state: "not_checked"` with an empty
`counterparts` list: the route is now orphaned (0 counterparts), which is why
it carries no finding of its own.

## 1. Headless Claude Code, three arms (about $0.4 sub per arm)

Two traps live in the harness itself, both found on 2026-09-09 and both able to
make a working channel read as dead:

* **An edit the model makes with Bash starts nothing.** Claude Code opens a
  file into a plugin's language server only when the **Edit or Write tool**
  touches it — that open is what starts the server, lazily — and a `sed -i`
  through Bash never reaches it. The PostToolUse hook matches the same three
  tools. So on a machine where the model prefers Bash for small edits, both
  channels are correctly silent and nothing in the transcript says why. Every
  arm is graded on the tool the model actually used, and a Bash edit is an
  invalid run to re-roll, not a result.
* **`--disallowedTools` turns the diagnostic attachment off.** The obvious way
  to force the Edit tool is to forbid Bash. An arm run that way collects no
  diagnostics at all: with the flag present Claude Code never calls
  `getLSPDiagnosticAttachments`, so a server that published exactly the right
  thing delivers nothing and the arm reads as a product failure. Ask for the
  Edit tool in the prompt instead.

Guards, from the harness rules: identical `allowedTools` in every arm, the
grader reads the session transcript rather than `stream-json` (diagnostics are
absent from the stream), and no arm is scoped by the client's workspace folder.
Work on a scratch copy of the workspace — the arms edit files.

```
cd "$WS"
TASK='Use the Edit tool (do not use Bash or sed) to change the response of the
GET route in <service>/<file>.ts so it no longer returns <field>, then stop.'
TOOLS='Read,Edit,Bash'

# arm A, control: no plugin, no channel
claude -p "$TASK" --allowedTools "$TOOLS" > "$WS/../smoke-control.log"

# arm B, hook: the plugin, hook channel
CARRICK_CHANNEL=hook CARRICK_LOG=/tmp/hook-arm.log \
  claude -p "$TASK" --allowedTools "$TOOLS" \
  --plugin-dir <carrick checkout>/plugin --debug --debug-file /tmp/hook-arm.dbg \
  > "$WS/../smoke-hook.log"

# arm C, LSP: the same plugin, LSP channel pinned, task ends on the edit
CARRICK_CHANNEL=lsp CARRICK_LOG=/tmp/lsp-arm.log \
  claude -p "$TASK" --allowedTools "$TOOLS" \
  --plugin-dir <carrick checkout>/plugin --debug --debug-file /tmp/lsp-arm.dbg \
  > "$WS/../smoke-lsp.log"
```

Grade from the transcript, not the logs above. The config directory is not
always `~/.claude`:

```
TRANSCRIPT=$(ls -t "${CLAUDE_CONFIG_DIR:-$HOME/.claude}"/projects/*/*.jsonl | head -1)
# arm health: the run is only valid if the model used the Edit tool
jq -r 'select(.type=="assistant") | .message.content[]? | select(.type=="tool_use") | .name' "$TRANSCRIPT" | sort | uniq -c
jq -c 'select(.type=="attachment" and .attachment.type=="hook_additional_context")' "$TRANSCRIPT" | wc -l
jq -c 'select(.type=="attachment" and .attachment.type=="diagnostics")' "$TRANSCRIPT" | wc -l
```

Four lines say where an arm stopped, which is what turns "delivered nothing"
into a diagnosis:

| line | file | means |
|---|---|---|
| `tool_dispatch_start tool=Edit` | `--debug-file` | the edit went through the tool, so the channel was reachable |
| `carrick-lsp: start pid` | `CARRICK_LOG` | Claude Code started the server |
| `check <file> -> N diagnostic(s)` | `CARRICK_LOG` | the server published |
| `LSP Diagnostics: Registering N diagnostic file(s)` | `--debug-file` | Claude Code took them |

Passes when arm B shows hook contexts and no diagnostic attachment, arm C shows
the reverse, and arm A shows neither. Arm C is also the one-turn-late test: a
run that ends on the edit either carries the diagnostic into the final turn or
does not, and the transcript says which.

**Run on 2026-09-09**, Claude Code 2.1.265, the checkout binary at `d0f2dd1`
(0.3.49 without the release bump; it prints 0.3.48), on a two-repo workspace
built from `tests/fixtures/local-mode-workspace` with a real local index.

| arm | hook contexts | diagnostic attachments | expected |
|---|---|---|---|
| A control, no plugin | 0 | 0 | 0 / 0 |
| B `CARRICK_CHANNEL=hook` | **1** | 0 | 1 / 0 |
| C `CARRICK_CHANNEL=lsp` | 0 | **1** | 0 / 1 |

Both arms started the server, and in arm B it published nothing and said so —
`the hook channel owns delivery in this install (env), so this server publishes
nothing` — which is the one-channel rule working rather than a server that
failed. Arm B's context carried the workspace's real finding, and the model
raised it unprompted in its closing words:

> Unrelated to the task, but worth flagging: the Carrick hook noted a
> pre-existing method mismatch [...] That's a real bug worth a look, not
> something I introduced or fixed here.

Arm A ran under the earlier flag set (`--disallowedTools Bash`), which suppresses
the diagnostic collector; it has no plugin and no channel, so its 0 / 0 does not
depend on that.

## 2. The root guard (about $0.4 sub)

The trap the spike hit: `rootUri` follows the agent's shell, so a `cd` into one
service before the first edit used to root the server there and report nothing.

```
cd "$WS"
CARRICK_CHANNEL=lsp CARRICK_LOG="$WS/../lsp.log" claude -p \
  'Use Bash to cd into <service> and list its src directory, then use the Edit
   tool (not Bash) to add a doc comment in <service>/<file>.ts, then read the
   file back and stop.' \
  --allowedTools 'Read,Edit,Bash' --plugin-dir <carrick checkout>/plugin
grep 'using' "$WS/../lsp.log"
```

Passes when the log names the workspace root as the directory in use and the run
still produces diagnostics. A run where the CLI was invoked from the service
directory is a failure even if it printed something.

**Run on 2026-09-09, same workspace, with the contract broken before indexing**
(the fixture's `loader` -> `action` edit, so the consumer's GET meets a POST).
The trap fired and the guard held: the client's workspace folder arrived as the
service directory, not the workspace, and the server rejected it —

```
client workspace folder .../smoke-ws/inventory-svc has no .carrick/;
using .../smoke-ws (project_dir)
```

— then published three diagnostics for the consumer file, of which the model
received two errors naming the producer:

```
GET /api/v1/widgets/:encoded method_mismatch (no type verdict): this call uses
GET and the producer serves POST at /api/v1/widgets/:widgetId
```
## 3. Any LSP client, and then VS Code (free)

The server is the same in every editor, so most of what the VS Code smoke shows
can be seen without one, by driving `carrick lsp --stdio` the way a client does
(`initialize` with the workspace folder, `initialized`, `didOpen`) and printing
the `publishDiagnostics` notifications. Since carrick#854 that is a script:

```
node npm/carrick/scripts/lsp-probe.mjs --workspace $WS \
  --open $WS/user-service/src/routes/users.ts
```

One row per published URI, the server's log on stderr as it arrives, and a
non-zero exit when the server never started, published nothing, or named a
counterpart file that is not on disk. Add `--server <install>/node_modules/carrick/dist/server.js`
to ask the same question of an installed package, and `--json` to diff two runs.

**Run on 2026-09-08 against the three-repo workspace**, opening the producer and
one consumer:

- the producer file: an error diagnostic on the route line, `GET /api/users
  type_mismatch: ... Response not assignable to number`, with the consumer's
  site as `relatedInformation`;
- the consumer file, **which the client never opened**: its own diagnostic
  naming the producer;
- an information diagnostic at line 1 of each service's file carrying the
  boundary;
- 35 ms and 27 ms for the two checks, root taken from the client and logged.

**Run on 2026-09-10, `--server` pointed at the shipped
`carrick@0.3.58`'s `node_modules/carrick/dist/server.js`** (the npm-installed
package from the run above, not this checkout's `src/server.ts`), `CARRICK_BIN`
set to that same install so the server's own child process runs the shipped
binary rather than PATH's 0.3.53 (`cli.js` reads `CARRICK_BIN` before falling
back to `carrick` on PATH, confirmed by reading the built file). Against the
same scratch copy of `tests/fixtures/local-mode-workspace`. No editor was
opened for this run. VS Code, Cursor and Windsurf are not installed on this
machine, and Zed, the one editor that is, was not launched. This probe stands
in for the editor rows in TEST-PLAN.md §4, and is not a substitute for actually
running one.

Clean state (before the fixture's method-mismatch edit), opening both files:
one row each, `catalog-web/.../$widgetId.ts` and `inventory-svc/src/
inventory.ts`, one diagnostic apiece: `severity: information`, `code:
boundary`, no `relatedInformation`. This fixture is a clean producer/consumer
match (`check` agreed: `resolved` / `compatible`), so the boundary line is the
only thing there is to publish. Root logged as `root .../ws (client)`, taken
from the workspace folder the probe sent, as it should be for an editor that
opens the workspace root. `OK 2 file(s) published`, exit 0.

After the fixture's documented `loader` to `action` edit and `carrick refresh
--service catalog-web`, opening only the consumer file: 3 diagnostics on
`inventory-svc/src/inventory.ts`, two `severity: warning`, `code:
method_mismatch`, `GET /api/v1/widgets/:encoded method_mismatch (no type
verdict): this call uses GET and the producer serves POST at
/api/v1/widgets/:widgetId`, one per call site (lines 9 and 17), plus the
boundary information line. Warning, not error: `severityOf` in
`npm/carrick/src/diagnostics.ts` demotes a routing finding to warning when the
other side is not resolvable on disk, and this route now has 0 counterparts,
so this is the stated policy, not a regression against section 2's "two
errors" on a different (resolvable) fixture. No `relatedInformation` either:
the producer's route is orphaned, so there is no counterpart location to
attach; opened separately, the producer file showed only its own boundary
line, for the same reason. `OK 1 file(s) published`, exit 0 both times. The
consumer-side finding matches the fixture's own answer key
(`tests/fixtures/local-mode-workspace/README.md`) and the method-mismatch
wording recorded in section 2 above.

Then VS Code itself, which needs a machine with VS Code on it:

1. `cd plugin/vscode && npm install && npm run build && npx --yes @vscode/vsce package`
   (2026-09-08: 320 files, 460 KB).
2. Install the `.vsix`: `code --install-extension carrick-<version>.vsix`.
3. Open `$WS` as the workspace folder. Set `carrick.binary` if `carrick` is not
   on PATH for GUI applications.
4. Break a producer response type, run `carrick refresh --service <producer>`,
   then open the two consumer files.
5. Both consumer diagnostics appear in Problems with the producer as a related
   location, and the producer file carries the same finding plus the boundary
   line.

Record it once. That recording is the editor half of the delivery claim.

## What none of this proves

- Nothing here measures whether a pushed fact changes what an agent concludes.
  That needs terrain and a graded corpus, and it is a separate cell.
- Deletions still fire nothing (E17). A removed producer surfaces at the next
  session start or the next explicit `carrick check`.
