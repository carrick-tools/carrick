# Local mode output contract (`carrick.check/0`)

The wire shape `carrick touch --json` and `carrick check --json` print, and the
human form they print without `--json`. Pinned 2026-09-06. Anything reading
local mode — the Claude Code hook, the LSP shim, a script — reads this file.

Governed code: `src/local_mode/` (the emitter is `src/local_mode/contract.rs`).
The machine-checkable form of the same shape is
[`schemas/carrick-check-0.json`](./schemas/carrick-check-0.json).

## Compatibility rule

Fields may be ADDED. A field is never renamed or removed without telling every
reader first. A reader must ignore fields it does not know, and must treat an
absent optional field as "this run did not state it", never as a zero or a
falsehood — the same rule the index blob follows.

`schema` is the version marker. `carrick.check/0` is this document. A breaking
change bumps it to `carrick.check/1` and both are emitted for one release.

## The commands

| command | reads | writes | budget |
|---|---|---|---|
| `carrick index --workspace <dir>` | detected local repos, optional workspace overrides, authenticated hosted indexes, and Carrick Cloud, which classifies what the deterministic passes could not | `<dir>/.carrick/`, plus `.carrick/last-scan.json` | the paid scan; minutes, cold, and it ends by saying what it cost |
| `carrick index --detach` | the same | the same, plus `.carrick/scan-<id>.log` and `.carrick/scan-<id>.json` | returns at once |
| `carrick status [--json]` | local index, credential identity | nothing | < 300 ms |
| `carrick touch <file> [--json]` | local index, credential identity | nothing | < 300 ms |
| `carrick check <file> [--json]` | local index, credential identity | nothing | < 300 ms |
| `carrick check <file> --recheck` | the same, plus this repo's working tree and the blobs already on disk | nothing outside a temp directory it deletes | 10 s budget |
| `carrick refresh [--service <name>]` | local source and authenticated hosted indexes | `<dir>/.carrick/` | seconds |

`status` answers about the workspace and takes no file; it is what a surface
opening a session asks. `touch` and `check` never call a model and never call
the cloud. Without `--recheck` they never parse the file either: they read what
`index` already computed. `touch` answers "what is on the other side of what I
am editing"; `check` adds the contract verdicts. What either one FINDS never
moves the exit code: local mode is advisory, nothing blocks, and an index full
of mismatches exits 0. The one exception is `check` refusing to answer at
all — no index, the file outside the workspace, an index this build cannot
read — which exits 1, because a script asking whether this file's contracts
hold could not otherwise tell "no contract problems" from "no answer"
(carrick#1023 item 2). `touch` and `status` exit 0 even then: they are the
editor's and the session's reads, and an edit must never fail because an index
is missing. The refusal body is printed either way, so a reader parsing the
JSON is unaffected by the code.

`carrick check --recheck` is the one read that recomputes (carrick#1036). When
the file has changed since the index, it re-scans **that file's repo only** —
deterministic extraction and the sidecar's type capture, no model, nothing on
the wire — and re-runs the join over the blobs already in `.carrick/repos`
plus the cached hosted snapshot, so the verdicts it prints are about the
working tree rather than the tree the index was built on. It writes nothing:
`.carrick/index.json` is untouched and the temporary generation is deleted on
every path. The whole re-check has a budget (10 s, `CARRICK_RECHECK_BUDGET_MS`);
past it the answer is the indexed one, and the `recheck` block below says so.
A file the tree has not changed, a deleted file, and every `touch` are answered
from the index as before. A changed file that holds no row is re-checked like
any other: the row it might hold now is the reason to look.

`index` recomputes deterministic facts from the working tree and reads hosted
indexes using the credential stored by `carrick login` or `CARRICK_TOKEN`.
Unchanged files can replay the hosted model answers through the scanner's
incremental path. A changed file keeps its local facts and withholds hosted
model answers, including staged, uncommitted, untracked and deleted changes.
The hosted commit must exist locally and the analysis cache versions must
match. Cached framework detection, guidance and extraction config additionally
require the package manifest hash to match.

Hosted-only repositories participate in the existing matching and compiler
checks. They contribute counterparts with `repo: null` and `remote: owner/repo`,
and never contribute local file items or navigation paths.

Only `index` and `refresh` make hosted network requests. On a failed read, a cached
hosted snapshot can be used for the same credential and authenticated workspace;
the boundary states its date and the failure. A credential or account change
cannot reuse another workspace's snapshot. Read-only commands check the current
local credential identity before serving hosted answers; a different or missing
credential returns `index_unreadable` until the local index is rebuilt. A scoped
refresh retains other local
scans only when workspace paths and authenticated hosted inputs are unchanged;
otherwise it rebuilds the local services before joining them. Nothing uploads
local source or runs a model on this machine.

## JSON

```json
{
  "schema": "carrick.check/0",
  "file": "app/routes/orders.$id.ts",
  "repo": "/Users/dev/repos/webapp",
  "service": "webapp",
  "index_commit": "a1b2c3d4e5f6",
  "indexed_at": "2026-09-06T21:14:03Z",
  "scanner_version": "0.3.41",
  "changed_since_index": 3,
  "stale": true,
  "deleted": false,
  "items": [
    {
      "kind": "route",
      "method": "GET",
      "path": "/api/orders/:id",
      "line": 12,
      "col": 1,
      "source": "fact",
      "resolution_source": "file_based_route",
      "evidence": "loader export claimed by a file-route convention",
      "counterparts": [
        {
          "role": "consumer",
          "service": "admin-ui",
          "repo": "/Users/dev/repos/admin-ui",
          "file": "src/api/orders.ts",
          "line": 44
        }
      ],
      "verdict": {
        "state": "resolved",
        "result": "type_mismatch",
        "detail": "Type 'UsersResponse' is not assignable to type 'number'"
      },
      "direction": "response",
      "actual_type": "UsersResponse { users: UserV2[] }",
      "expected_type": "number"
    }
  ],
  "boundary": {
    "commit_hash": "a1b2c3d4e5f6",
    "files_attempted": 0,
    "files_lost": { "total": 0 },
    "unemitted_literal_candidates": 12,
    "consumers_not_resolved": { "total": 0 },
    "sdk_unresolved": { "total": 0 },
    "unknown_call_paths": { "total": 0 },
    "model_only_rows": 0,
    "model_rows_joined": 0,
    "model_contradictions_discarded": 0
  },
  "boundary_note": "candidates: not classified locally (no model runs on this machine). A route registered on a typed receiver (`app.get(\"/x\", h)`) and a call whose URL is built at the call site are classified by the model in the hosted index and are absent here: 12 route-literal call site(s) counted and unclassified in this service."
}
```

`boundary` is the `ServiceBoundary` block from `src/boundary.rs`, verbatim and
whole; the fields above are a sample of it, not its definition. `files_attempted`
is 0 on every local index, because a local index asks the model nothing.
`candidates_withheld_changed_files`, when present, counts files whose hosted
model answers were withheld because their working-tree bytes changed. An absent
counter means this scan did not record it.

### Top level

| field | type | meaning |
|---|---|---|
| `schema` | string | always `carrick.check/0` for this document |
| `file` | string | the queried file, relative to the repo that owns it |
| `repo` | string | the absolute path of that repo on this machine. `repo` + `file` is the path to open; `file` alone is what the index keys on |
| `service` | string | the service the file belongs to (`serviceName` from `carrick.json`, else the repo name) |
| `index_commit` | string | the commit that service was indexed at |
| `indexed_at` | string (RFC 3339) | when `index` (or the last `refresh` of this service) ran |
| `scanner_version` | string | the scanner release that wrote the index |
| `hosted` | object \| null | `{commit, indexed_at, scanner_version, project}` describing the hosted copy; its commit is separate from `index_commit`. Adds `source` (`"ci"` \| `"laptop"`), `uploaded_by` (GitHub login) and `dirty` (boolean) when the hosted row records them; each key is absent, never null, on a row that does not |
| `hosted_state` | string | `enriched`, `no_index_yet`, `not_connected`, `remote_unnamed`, `not_signed_in`, `version_mismatch`, `commit_missing`, or `read_failed`; the boundary explains any failed refresh alongside a retained copy. `not_connected` is the cloud's answer about a repo it was asked about; `remote_unnamed` is this machine unable to read `owner/repo` from the repo's git remote, so it asked about nothing |
| `hosted_checked_at` | string \| null | when repository metadata last answered for this workspace; absent when no authenticated copy exists |
| `changed_since_index` | int | files changed since `index_commit`: `git diff --name-only <commit>` plus what git does not track. Where git cannot answer at all (no repository, a commit a rebase dropped, no `git`), it falls back to whether the queried file's own mtime is newer than the index, and counts only that file. A rewrite that changes no bytes is not a change |
| `stale` | bool | this file is one of them, so its rows may not describe what is on disk now |
| `deleted` | bool | the file is in the index and no longer on disk |
| `items` | array | routes and calls the index holds for this file, in line order |
| `boundary` | object \| null | what this service's scan could not classify (`ServiceBoundary`, `src/boundary.rs`), verbatim |
| `boundary_note` | string | hosted provenance, replay limits or the reason enrichment is unavailable, with the counts the scan kept. Always present. |
| `boundary_lines` | string[] | the boundary as the CLI prints it, line by line: `boundary_note` first, then the counts. A reader rendering the boundary prints these bytes rather than re-wording the struct, so a hook and a terminal say the same sentence about the same number. |
| `recheck` | object \| absent | what a `--recheck` call did (below). Absent on every other read, which means the items are the indexed ones — the answer this document described before carrick#1036 |

### `recheck`

Present only when `--recheck` was passed AND the file had changed since the
index. Absent otherwise, including on a `--recheck` call for a file the tree
has not moved past: nothing needed re-judging, so nothing was.

| field | type | meaning |
|---|---|---|
| `ran` | string | `extraction+types`: the file was re-extracted, re-joined against the blobs the index holds, and at least one of its rows carries a type verdict. `extraction`: the same, and no row of this file carries one — nothing here pairs with anything, or its pairs were not both resolved, and each row's own `verdict.detail` says which. `none`: the items above are the indexed ones |
| `elapsed_ms` | int | wall time of the re-check, including one that missed its budget |
| `stale_since` | string (RFC 3339) | when the items above were computed. Present only on a `none` — otherwise the answer is now |
| `reason` | string | why the re-check did not run, in one sentence. Present only on a `none` |

The items in one answer are all fresh or all indexed, never a mixture, and this
block is the only thing that says which. `stale` and `changed_since_index` keep
their meaning either way: they describe the tree against the index, not the age
of the rows.

Locations come first and the boundary comes last: a reader that stops early has
read the facts, and a reader that reads to the end knows what is missing.

**A service with no `serviceName` is named differently here than in the hosted
index.** Locally it takes the name of its repo's DIRECTORY on this machine,
because that is what a scan with no CI environment records; in the cloud the
same service is named after its GitHub repository. The two agree whenever the
directory is the repo name, which is the ordinary case, and a workspace holding
two repos with one directory name is refused before anything is scanned rather
than silently overwritten.

### `items[]`

| field | type | meaning |
|---|---|---|
| `kind` | `"route"` \| `"call"` | a route this file serves, or a call this file makes |
| `method` | string | HTTP method, GraphQL kind, or socket direction |
| `path` | string | route path, GraphQL field, socket event, or pub/sub topic |
| `line` | int \| null | 1-based line, when the index recorded one |
| `col` | int \| null | 1-based column, when the index recorded one |
| `source` | `"fact"` \| `"candidate"` | `fact` = a deterministic pass stated it; `candidate` = a replayed hosted model answer. Local facts remain authoritative when a model answer contradicts them. |
| `resolution_source` | string \| null | the wire value from the index blob: `file_based_route`, `imported_member`, `model`, … `null` = this row does not state it |
| `evidence` | string \| null | one line naming what the row was read off |
| `counterparts` | array | the other side of the contract, across every repo in the workspace |
| `verdict` | object \| null | `null` from `touch` always; `check` fills it in |
| `direction` | `"request"` \| `"response"` | which half of the contract the two type texts below belong to. Absent from `touch`, from a row nothing was compared for, and from a row whose producer is gone from disk |
| `expected_type` | string | the printed type the READING side of `direction` declares |
| `actual_type` | string | the printed type the SENDING side of `direction` states |

### The two types, and which side holds which

A direction is one assignability check, and the two words name its two ends:
`actual_type` is the SOURCE — what the sending side states — and
`expected_type` is the TARGET, what the reading side declares. They are the
same two ends the verdict's own `detail` names when the compiler says one type
is "not assignable to" another. Which service holds which flips with the
direction, which is why the direction is carried rather than inferred:

| `direction` | `actual_type` (source) | `expected_type` (target) |
|---|---|---|
| `request` | what the consumer sends | the producer's declared request type |
| `response` | the producer's response type | what the consumer's call site reads |

Both are printed type text, capped at 200 characters with `...` where the cap
cut it, and both are absent unless the index holds them: nothing was compared,
the check stated no direction, or the pair's types never resolved. An absent
field is "this run did not state it", never a type of `unknown`. A mismatch
reached through a published client package states no direction, because the
two strings it carries are the package and the service rather than types.

A surface rendering a row that holds all three states them in one sentence
(carrick#1033) — the LSP diagnostic and the post-edit hook line both print:

```
GET /api/users: response is UsersResponse { users: UserV2[] }, consumer at notification-service server.ts:25 reads number
```

The direction's payload "is" the actual type, and the side that reads it is
named with its location. A `request` swaps the roles, so the consumer's sent
type leads and the producer expects. Where the row is itself the reading side,
the sending side is named instead and the row says "this call" or "this route".
Where the index holds no types, the sentence is the verdict's result and detail
as before.

### `items[].counterparts[]`

| field | type | meaning |
|---|---|---|
| `role` | `"producer"` \| `"consumer"` \| `"peer"` | what the counterpart is. `peer` is a shared external contract: both sides call the same third party, and neither serves the other. |
| `service` | string | the counterpart's service |
| `remote` | string \| null | GitHub `owner/repo` when the counterpart is hosted-only; `repo` is null in that case |
| `repo` | string \| null | the absolute path of the counterpart's repo on this machine. `repo` + `file` opens it; null when the index no longer holds that repo |
| `file` | string | the counterpart's file, relative to ITS OWN repo, which is a different repo from the queried file's |
| `line` | int \| null | 1-based line, when the index recorded one |

### `items[].verdict`

`null` from `touch`, and from `check` on a row nothing was compared for.

| field | type | meaning |
|---|---|---|
| `state` | `"resolved"` \| `"unresolved"` \| `"not_checked"` | the type layer's word, and only that |
| `result` | `"compatible"` \| `"type_mismatch"` \| `"method_mismatch"` \| `"producer_removed"` \| null | null where `state` is the whole statement |
| `detail` | string | one line, written to be read by a model |

`state` uses the SAME three words with the SAME meanings as `verdict_state` on
the PR-result payload (carrick#727, carrick#731), because one agent reads both
contracts in one session:

- **`resolved`** — the compiler reached a verdict, with no `any`, `unknown` or
  error on either side. `result` is `compatible` or `type_mismatch`.
- **`unresolved`** — a verdict was attempted and a side of the pair did not
  resolve to a usable type, so nothing is claimed. `result` is null.
- **`not_checked`** — no type verdict bears on this row: nothing pairs with it,
  the pair was never compared, or the finding is a routing fact rather than a
  type one (`method_mismatch`, `producer_removed`).

**`state` never says anything about freshness.** Whether the tree has moved
since the index is `stale` and `changed_since_index` at the top level, said
once for the file. The verdict's `detail` repeats it in words for a reader that
sees one row and not the envelope.

`producer_removed` is the deleted-route case: the file is gone from disk, the
index still holds the route, and its consumers are listed as counterparts.

### Errors

Exit code is 0 for every read-only command, including failure: a hook must
never fail an edit. A missing or unreadable index prints one line to stderr
saying what to do, and with `--json` this to stdout:

```json
{
  "schema": "carrick.check/0",
  "error": "index_unreadable",
  "message": "/w/.carrick/index.json was written by a different scanner (index format 2, this build reads 3). Re-run `carrick index`."
}
```

A caller parsing JSON therefore always gets JSON. Without `--json` the sentence
on stderr is the whole answer: printing a JSON body into a human's terminal
would be noise, not an error report.

`error` is the code and `message` is the sentence beside it, on every refusal.
The code is coarse on purpose — three values cover everything — so a surface
showing a human why there is no answer prints `message`, never the code
(carrick#1009). The language server publishes it as one Information diagnostic
on line 1 of the open file, and the session-start line repeats it.

| `error` | meaning |
|---|---|
| `not_indexed` | no `.carrick/` above this file — run `carrick index --workspace <dir>` |
| `not_in_workspace` | the file is not under any repo the workspace lists |
| `index_unreadable` | `.carrick/` exists and could not be read (a version mismatch, a truncated write) |

## Human output

Without `--json`, the same content in the same order: the file and its service,
then one block per item (location, method and path, source label, counterparts
with their locations), then the staleness line, then the boundary. Written for
a model reading a terminal, so every location is a path a reader can open and
no line needs a legend.

```
app/routes/api.v1.widgets.$widgetId.ts (catalog-web, indexed at 4b96017)

  route  GET /api/v1/widgets/:widgetId  line 11  [fact: file_based_route]
    consumer  inventory-svc  src/inventory.ts:9
    consumer  inventory-svc  src/inventory.ts:17
    verdict   type_mismatch — GET /api/v1/widgets/:widgetId -> Response not
  assignable to GET /api/v1/widgets/:encoded -> Response
    Types of property 'activeCount' are incompatible.

changed since index: 1 file(s); this file is one of them, so its rows are
unresolved since your edit
boundary (catalog-web): candidates: not classified locally (no model runs on
this machine). A route registered on a typed receiver (`app.get("/x", h)`) and
a call whose URL is built at the call site are classified by the model in the
hosted index and are absent here: 0 route-literal call site(s) counted and
unclassified in this service.
  catalog-web at 4b96017: 0 file(s) sent to the analyzer
```

## On-disk layout

`carrick index --workspace <dir>` writes `<dir>/.carrick/`:

| path | what |
|---|---|
| `.carrick/.gitignore` | `*` — the directory ignores itself, so no user file has to change |
| `.carrick/repos/*.json` | local per-service blobs, including eligible replayed hosted model answers |
| `.carrick/hosted/snapshot.json` | authenticated hosted data, metadata and credential fingerprint; no bearer token |
| `.carrick/index.json` | the joined read model `touch` and `check` answer from: every repo's absolute path, its services with their commits and boundaries, and per file the rows with their counterparts and verdicts |
| `.carrick/build-*/` | transient per-run blobs and join result, removed when the build finishes |
| `.carrick/scan-<id>.log` | everything a detached build printed: its banner, its per-service lines, its map, and any error. Kept after the scan ends — it is the record of a run nobody watched |
| `.carrick/scan-<id>.json` | what that build is doing NOW: phase, service, counts, pid, when it started, and `spend` once an upload has come back with a figure. Removed when the scan finishes; left with `"status": "failed"` when it fails, and left as-is when the process is killed, which is how `carrick status` can say a scan stopped part-way |
| `.carrick/last-scan.json` | what the last **paid** scan cost: one entry per repo `carrick index` scanned, each carrying the cloud's `carrick.scan-spend/0` block. Written as each figure lands, so a run killed after paying still leaves it. Beside the index rather than in it, because `carrick refresh` rebuilds the read model from scratch and a first paid run may be killed before there is one |

`index.json` is derived: deleting it and re-running `carrick index` reproduces
it. Nothing outside `src/local_mode/` reads it, and its internal shape is not
this contract — only the command output above is.

`<dir>/carrick-workspace.json` is an optional overrides file. Shared Rust
workspace detection selects a configured repo, a workspace manifest, immediate
sibling repos, or the current repo. The CLI can propose the immediate parent
without selecting it. Overrides add or exclude repo paths, relative to this
file or absolute:

```json
{ "repos": ["../shared-client"], "exclude": ["./archived-service"] }
```

## What `touch` and `check` require

Both take exactly one file. Neither answers for a workspace: with no path they
print `carrick touch needs a file path` and exit 2, which is a usage error and
not a read, and nothing is written to stdout. That is why `file`, `repo` and
`service` are required in the schema: every `carrick.check/0` response is about
one file, and a reader that always has one should not have to defend against a
response that does not.

The workspace question has its own command and its own schema, below.

## `carrick status` — the workspace, with no file in the question

What a surface opening a session asks: what is indexed, at which commit, how
far each repo has moved since, and what each service could not classify. Same
rules as the other reads: local index and credential identity, under 300 ms,
and exit 0 whatever it finds — including a refusal, unlike `check`.

Its refusal is written for a command that takes no file, and it never names the
scan that is already running: a workspace whose first paid scan is in flight is
told that, not told to start one (carrick#1023 item 1). The running scan is
printed above the sentence, and carried in `running_scans` on the error body.

`--json` prints **`carrick.status/0`**
([`schemas/carrick-status-0.json`](./schemas/carrick-status-0.json)):

```json
{
  "schema": "carrick.status/0",
  "workspace": "/Users/dev/repos",
  "indexed_at": "2026-09-06T21:14:03Z",
  "scanner_version": "0.3.41",
  "repos": [
    {
      "repo": "/Users/dev/repos/webapp",
      "name": "webapp",
      "changed_since_index": 5,
      "outside_every_service": 2,
      "stale_files": ["carrick.json", ".github/workflows/carrick.yml"],
      "stale_files_truncated": false
    }
  ],
  "services": [
    {
      "service": "webapp",
      "repo": "/Users/dev/repos/webapp",
      "index_commit": "a1b2c3d4e5f6",
      "indexed_at": "2026-09-06T21:14:03Z",
      "routes": 157,
      "calls": 12,
      "changed_since_index": 3,
      "stale_files": ["app/routes/orders.$id.ts"],
      "stale_files_total": 3,
      "stale_files_truncated": false,
      "boundary": { "commit_hash": "a1b2c3d4e5f6", "files_attempted": 0 },
      "boundary_note": "candidates: not classified locally ...",
      "boundary_lines": ["boundary (webapp): candidates: not classified ..."]
    }
  ]
}
```

| field | meaning |
|---|---|
| `workspace` | the folder holding `carrick-workspace.json` and `.carrick/` |
| `services[].repo` | absolute; services of one repo share a commit, and this is what says so |
| `services[].changed_since_index` | the exact number of changed and untracked files **this service's scan reads**: its `directory` and its `include` roots. A file two services both include counts for both; a file no service reads counts for neither, and is on the repo's entry instead |
| `services[].stale_files` | up to 50 of them, repo-relative; `stale_files_total` is always exact and `stale_files_truncated` says which you are looking at |
| `repos[]` | one per indexed repo: `changed_since_index` is the whole tree, `outside_every_service` is how many of those no service reads, and `stale_files` is up to 50 of THOSE. A workflow file, a lockfile or an editor's settings belongs here and not to every service in the monorepo |
| `running_scans[]` | a build happening right now, from `carrick index --detach`: `scan_id`, `pid`, `started_at`, `phase` (`indexing <repo>`, `joining the workspace`), `progress` (service, `files` or `intents`, done of total), `notice` (why a running scan is slow, in the scan's words, cleared by the next phase) and `infer`. `status` is `running`, `finished` or `failed`: a scan that ends rewrites its record rather than removing it, with `finished_at`, so a caller polling until it is over terminates on a word; the next build clears a finished one. A failed record carries `error`, whose first line is the reason in one sentence (the one the scan stated, or why the build refused) and whose later lines are an excerpt of the scan's own log; the human `status` prints the first line only, one line per scan, and names `.carrick/scan-<id>.log` once for the rest. Absent in the ordinary case. A `status` with no index at all still carries it, on the error body, because "no index" and "one is being built" are different answers |
| `last_scan` | what the last paid scan cost: `updated_at`, and `scans[]` of `{ repo, spend }` where `spend` is the cloud's `carrick.scan-spend/0` block (`usd`, `priced`, `unpriced_models`, token counts, `first_index_ceiling_usd` / `first_index_remaining_usd`, `monthly_allowance_usd` / `monthly_remaining_usd`, `period`). Absent until a paid scan has run, and carried on the error body too — a first paid run killed before it wrote an index still spent the money. Two rules the CLI's own line follows and any other reader should: `priced: false` means print nothing about money at all, because one unpriced model makes every dollar figure in the month an under-count; and a null amount is "not set", not "unlimited", so the clause is left out rather than shown empty |
| `services[].boundary.candidates_awaiting_model` | candidates no model has been asked about, so `0 route(s) 0 call(s)` from a free pass is distinguishable from a service with no API in it. Zero after a scan that ran the model; absent on an index written before the count existed |
| `services[].boundary_lines` | the same pre-rendered lines `check` and `touch` carry |
| `services[].hosted`, `services[].hosted_state` | the same provenance and replay state as `check` |
| `hosted_checked_at` | when repository metadata last answered for this workspace |
| `repos_detected_by` | `carrick_json`, `workspace_manifest`, `siblings`, `single_repo`, or `workspace_overrides` |
| `repos_added`, `repos_excluded` | explicit workspace override paths; absent when empty |

Errors are the same three, with the same `message` beside the code, under this
schema: `{ "schema": "carrick.status/0", "error": "not_indexed", "message": "..." }`.

## Out of scope in this version

- Local model calls and uploads.
- Per-file incremental extraction and a persistent process.
- Background hosted refresh timers or changes to the MCP tool surface.
