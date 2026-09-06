# Smokes for the hook and the LSP shim

Written 2026-09-06, before the `carrick` CLI exists. Everything in the test
suite runs against fixture payloads and a fake binary, so none of it proves the
plugin against a real index. These are the runs that do. They belong to whoever
schedules the work after carrick#708 and carrick#710 land; the headless arms
spend money and are owner-gated.

Set once for every command below:

```
export PLUGIN=/path/to/carrick/plugin
export WS=/path/to/a/workspace/with/several/repos
```

## 0. The binary, before anything else (free)

```
cd "$WS"
carrick index --workspace "$WS"
carrick check <a file with a route> --json | python3 -m json.tool | head -40
```

Read the payload against `docs/schemas/carrick-check-0.json`. Then run the
plugin's suite with the real binary in place of the fake one:

```
cd "$PLUGIN"
CARRICK_BIN=$(command -v carrick) npm test
```

The fixtures still drive most of the suite; what this proves is that the real
binary's argv and exit behaviour match what `src/cli.ts` expects. Any difference
is a contract question for carrick#708, not a patch here.

Then one hook call by hand, which is the whole PostToolUse path:

```
cd "$WS"
echo '{"tool_name":"Edit","cwd":"'"$WS"'","tool_input":{"file_path":"'"$WS"'/<service>/<file>.ts"}}' \
  | node "$PLUGIN/src/hook/post-edit.ts"
```

Expect one JSON object with `additionalContext`, or nothing at all when the
index holds no rows and no boundary for that file. Time it: the hook's own work
is under 300 ms, and the CLI's share is the number in the log line
(`CARRICK_LOG_QUIET=0` puts it on stderr).

## 1. Headless Claude Code, three arms (about $1, owner-gated)

Guards, from the harness rules: identical `allowedTools` in every arm, the
grader reads the session transcript rather than `stream-json` (diagnostics are
absent from the stream), and no arm is scoped by the client's workspace folder.

```
cd "$WS"
TASK='Change the response of the GET route in <service>/<file>.ts so it no longer returns `email`, then stop.'
TOOLS='Read,Edit,Write,Bash,Grep,Glob'

# arm A, control: no plugin, no channel
claude -p "$TASK" --allowedTools "$TOOLS" > "$WS/../smoke-control.log"

# arm B, hook: the plugin, hook channel
CARRICK_CHANNEL=hook claude -p "$TASK" --allowedTools "$TOOLS" \
  --plugin-dir "$PLUGIN" > "$WS/../smoke-hook.log"

# arm C, LSP: the same plugin, LSP channel pinned, task ends on the edit
CARRICK_CHANNEL=lsp claude -p "$TASK" --allowedTools "$TOOLS" \
  --plugin-dir "$PLUGIN" > "$WS/../smoke-lsp.log"
```

Grade from the transcript, not the logs above:

```
TRANSCRIPT=$(ls -t ~/.claude/projects/*/*.jsonl | head -1)
# hook contexts
jq -c 'select(.type=="attachment" and .attachment.type=="hook_additional_context")' "$TRANSCRIPT" | wc -l
# diagnostic attachments
jq -c 'select(.type=="attachment" and .attachment.type=="diagnostics")' "$TRANSCRIPT" | wc -l
# and which turn Carrick first reached the model
jq -c 'select(.type=="attachment") | .attachment.type' "$TRANSCRIPT" | head
```

Those two attachment types are the counters the spike's grader used
(`harness/grade.mjs`); a plain grep over the transcript also matches the task
text and file contents, so it is not the instrument. Diagnostics never appear in
`stream-json` at all.

Passes when arm B shows hook contexts and no diagnostic attachment, arm C shows
the reverse, and arm A shows neither. Arm C is also the one-turn-late test: a
run that ends on the edit either carries the diagnostic into the final turn or
does not, and the transcript says which.

## 2. The root guard (about $0.30, owner-gated)

The trap the spike hit: `rootUri` follows the agent's shell, so a `cd` into one
service before the first edit used to root the server there and report nothing.

```
cd "$WS"
CARRICK_CHANNEL=lsp CARRICK_LOG="$WS/../lsp.log" claude -p \
  'cd <service>, read <file>.ts, then edit its route response and stop.' \
  --allowedTools 'Read,Edit,Bash' --plugin-dir "$PLUGIN"
grep 'has no .carrick/; using' "$WS/../lsp.log"
```

Passes when the log names the workspace root as the directory in use and the run
still produces diagnostics. A run where the CLI was invoked from the service
directory is a failure even if it printed something.

## 3. VS Code, recorded once (free)

1. `cd "$PLUGIN/vscode" && npm install && npm run build && node bundle-server.mjs`
2. `npx --yes @vscode/vsce package`, then install the `.vsix` into VS Code.
3. Open `$WS` as the workspace folder and set `carrick.binary` if `carrick` is
   not on PATH.
4. Break a producer response type, run `carrick refresh --service <producer>`,
   then open the two consumer files.
5. Both consumer diagnostics appear in Problems with the producer as a related
   location, and the producer file carries the same finding plus the boundary
   line.

Record it once. That recording is the editor half of the delivery claim, and the
same setup is the second arm of the producer-shape measurement cell if that cell
is ever run.

## What none of this proves

- Nothing here measures whether a pushed fact changes what an agent concludes.
  That needs terrain and a graded corpus, and it is a separate cell.
- Deletions still fire nothing (E17). A removed producer surfaces at the next
  session start or the next explicit `carrick check`.
