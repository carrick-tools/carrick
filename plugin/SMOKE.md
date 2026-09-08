# Smokes for the hook and the LSP shim

The plugin's own suite runs against fixture payloads and a fake binary, so none
of it proves anything against a real index. These are the runs that do. Section
0 and section 3 are free; sections 1 and 2 spend a little on model calls.

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

## 1. Headless Claude Code, three arms (about $1, sub)

Guards, from the harness rules: identical `allowedTools` in every arm, the
grader reads the session transcript rather than `stream-json` (diagnostics are
absent from the stream), and no arm is scoped by the client's workspace folder.
Work on a scratch copy of the workspace — the arms edit files.

```
cd "$WS"
TASK='Change the response of the GET route in <service>/<file>.ts so it no longer returns `email`, then stop.'
TOOLS='Read,Edit,Write,Bash,Grep,Glob'

# arm A, control: no plugin, no channel
claude -p "$TASK" --allowedTools "$TOOLS" > "$WS/../smoke-control.log"

# arm B, hook: the plugin, hook channel
CARRICK_CHANNEL=hook claude -p "$TASK" --allowedTools "$TOOLS" \
  --plugin-dir <carrick checkout>/plugin > "$WS/../smoke-hook.log"

# arm C, LSP: the same plugin, LSP channel pinned, task ends on the edit
CARRICK_CHANNEL=lsp claude -p "$TASK" --allowedTools "$TOOLS" \
  --plugin-dir <carrick checkout>/plugin > "$WS/../smoke-lsp.log"
```

Grade from the transcript, not the logs above:

```
TRANSCRIPT=$(ls -t ~/.claude/projects/*/*.jsonl | head -1)
jq -c 'select(.type=="attachment" and .attachment.type=="hook_additional_context")' "$TRANSCRIPT" | wc -l
jq -c 'select(.type=="attachment" and .attachment.type=="diagnostics")' "$TRANSCRIPT" | wc -l
jq -c 'select(.type=="attachment") | .attachment.type' "$TRANSCRIPT" | head
```

Passes when arm B shows hook contexts and no diagnostic attachment, arm C shows
the reverse, and arm A shows neither. Arm C is also the one-turn-late test: a
run that ends on the edit either carries the diagnostic into the final turn or
does not, and the transcript says which.

## 2. The root guard (about $0.30, sub)

The trap the spike hit: `rootUri` follows the agent's shell, so a `cd` into one
service before the first edit used to root the server there and report nothing.

```
cd "$WS"
CARRICK_CHANNEL=lsp CARRICK_LOG="$WS/../lsp.log" claude -p \
  'cd <service>, read <file>.ts, then edit its route response and stop.' \
  --allowedTools 'Read,Edit,Bash' --plugin-dir <carrick checkout>/plugin
grep 'using' "$WS/../lsp.log"
```

Passes when the log names the workspace root as the directory in use and the run
still produces diagnostics. A run where the CLI was invoked from the service
directory is a failure even if it printed something.

## 3. Any LSP client, and then VS Code (free)

The server is the same in every editor, so most of what the VS Code smoke shows
can be seen without one, by driving `carrick lsp --stdio` the way a client does
(`initialize` with the workspace folder, `initialized`, `didOpen`) and printing
the `publishDiagnostics` notifications.

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

Then VS Code itself, which needs a machine with VS Code on it:

1. `cd plugin/vscode && npm install && npm run build && npx --yes @vscode/vsce package`
   (2026-09-08: 320 files, 460 KB).
2. Install the `.vsix`: `code --install-extension carrick-0.0.1.vsix`.
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
