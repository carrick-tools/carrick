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
| `carrick index --workspace <dir>` | detected local repos, optional workspace overrides, authenticated hosted indexes, and Carrick Cloud, which classifies what the deterministic passes could not | `<dir>/.carrick/`, plus `.carrick/last-scan.json` | the scan that builds the index, and the one a first run makes; minutes, cold |
| `carrick index --detach` | the same | the same, plus `.carrick/scan-<id>.log` and `.carrick/scan-<id>.json` | returns at once |
| `carrick index --dispatch` | the same | `.carrick/jobs.json`, and no index — unless there was nothing to hand over, in which case it indexes here and says so (carrick#1251) | returns when the prompts are built, minutes on a large repo |
| `carrick resume` | `.carrick/jobs.json`, Carrick Cloud, local source | `<dir>/.carrick/` | minutes; the analysis is already done |
| `carrick status [--json]` | local index, credential identity, and Carrick Cloud when a job is recorded | nothing | < 300 ms, plus one network read for each job recorded, in turn |
| `carrick touch <file> [--json]` | local index, credential identity | nothing | < 300 ms |
| `carrick check <file> [--json]` | local index, credential identity | nothing | < 300 ms |
| `carrick check <file> --recheck` | the same, plus the file's service in this repo's working tree and the blobs already on disk | `.carrick/working/` and `.carrick/judged/` (replaced by the next index) and a temp directory it deletes | 10 s budget |
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
the file has changed since the index, it re-scans **the file's own service**
and the services that hold an operation reading a type declared in that file
(`recheck.scope.reread`) — deterministic extraction and the sidecar's type
capture, no model, nothing on the wire — and re-runs the join over the blobs
already in `.carrick/repos` plus the cached hosted snapshot, so the verdicts it
prints are about the working tree rather than the tree the index was built on.
Every other service is judged at its newest known version: the copy the last
re-check of it left in `.carrick/working/<index generation>/`, when its edits
since the index are exactly the ones that copy was read for, else the index's
own (named in `recheck.scope.stale`). It never writes `.carrick/index.json` or
`.carrick/repos`; the temporary generation is deleted on every path. The one thing
it keeps is the compiler rows of a service it carried and had to walk, in
`.carrick/positions/`; a later check or re-check reuses them only while the build,
the commit and every file's hash are the ones they were walked at (carrick#3245). The type
checker's finished answers are kept per index in
`.carrick/judged/<index generation>/`, written by `index` and `refresh` and
read by every re-check, so an edit that moves no type skips the checker's
dependency install. A new index replaces both folders. The whole re-check has a
budget (10 s, `CARRICK_RECHECK_BUDGET_MS`); past it the answer is the indexed
one, and the `recheck` block below says so (`deadline: true`). That answer is
not a check of the edit, so it is never a clean result (carrick#3860): the
terminal says so before any row, naming the index commit it answered from,

```
re-check: not checked, so these verdicts do not include your edits and are from the index at 1a2b3c4 (computed 2026-11-11T09:12:00Z). The re-scan ran past the budget.
```

and `check <file> --recheck` exits 2, as a check that covered nothing does. A
`known_miss` answer (below) prints and exits the same way.
A re-check that missed its budget is remembered in
`.carrick/recheck-misses.json`, per service and against the index it ran on
(its `indexed_at`). Until `refresh` or `index` writes a new index, a later
`--recheck` of a file in that service starts no re-scan: it answers at once with
the indexed verdicts and the new functions from the parse, and its block
carries `known_miss: true`. Another service of the same repo is still
re-checked. On such a service `carrick check --changed` is the fresh read. A
service where the re-check fits never misses and is never recorded. A miss
records the budget it ran under (`budget_ms`) and silences only a re-check
whose own budget is no longer: a miss at ten seconds does not switch off a
re-check given three minutes. A misses file written before that field existed
reads as no misses.
`--no-rescan` (with `--recheck`, on one file or on `--each`) asks the question
"does this edit need a re-scan" and never starts one. A file that needs none
is answered as without the flag: unchanged or gone, an edit that reads the
same, no service reads it, a known miss. Any other file comes back with
`ran: none`, the indexed rows, `new_functions`, and `deferred: true`; nothing
is staged and no miss is recorded. The caller makes the re-check itself later
with `--recheck` alone.
`carrick check --each <file>... --recheck --json` answers several files in one
process, each element of the printed JSON array the body `check <file>
--recheck --json` prints for that file alone; the post-edit hook asks it for a
patch that touches several files.
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

Only `index`, `refresh` and `resume` make hosted network requests, and `status`
when a job is recorded. On a failed read, a cached
hosted snapshot can be used for the same credential and authenticated workspace;
the boundary states its date and the failure. A credential or account change
cannot reuse another workspace's snapshot. Read-only commands check the current
local credential identity before serving hosted answers; a different or missing
credential returns `index_unreadable` until the local index is rebuilt. A scoped
refresh retains other local
scans only when workspace paths and authenticated hosted inputs are unchanged;
otherwise it rebuilds the local services before joining them. `refresh` uploads
nothing. `index` sends Carrick Cloud the source of the files the deterministic
passes could not classify and of the functions it writes intents for, and
uploads the index it builds. No model runs on this machine.

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
    "consumers_not_resolved": { "total": 0 },
    "sdk_unresolved": { "total": 0 },
    "unknown_call_paths": { "total": 0 },
    "model_only_rows": 0,
    "model_rows_joined": 0,
    "model_contradictions_discarded": 0
  },
  "boundary_note": "candidates: not classified locally (no model runs on this machine). A route registered on a typed receiver (`app.get(\"/x\", h)`) and a call whose URL is built at the call site are classified by the model in the hosted index and are absent here."
}
```

`boundary` is the `ServiceBoundary` block from `src/boundary.rs`, verbatim and
whole; the fields above are a sample of it, not its definition. `files_attempted`
is 0 on every service `refresh` scans, because `refresh` asks the model nothing.
`candidates_withheld_changed_files`, when present, counts files whose hosted
model answers were withheld because their working-tree bytes changed. An absent
counter means this scan did not record it.

`model_rows_carried` and `model_rows_not_carried` say what a no-model scan did
with the rows of those files (carrick#2894). A row whose call sits on a line the
edit left alone is carried: it moves to its new line and its types are read from
the working tree. Every other row is counted in `model_rows_not_carried` as
`file:line reason`, and is not checked. The reasons are
`registration_line_changed` (the edit replaced the registration line),
`no_matching_call` (no single call on that line matches the one the answer
named), `hint_outside_registration_changed` (a handler named at the
registration changed, so a type read there could come from the wrong function),
`carried_untyped` (carried, but the send line was edited, so there is no
response type), `old_source_unavailable` (this clone does not hold the indexed
commit's copy of the file) and `not_carried_<field>` (GraphQL, pub/sub and
dispatch rows are not carried yet). Both counters are absent when the tree has
no edited file with rows to carry. Carried rows are never written to the
stored index.

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
| `stale` | bool | this file is one of them, so its rows may not describe what is on disk now. False for an edit the scan reads the same (comments, layout): the rows stand, `changed_since_index` still counts the file, and `recheck.reason` says why |
| `deleted` | bool | the file is in the index and no longer on disk |
| `items` | array | routes and calls the index holds for this file, in line order |
| `boundary` | object \| null | what this service's scan could not classify (`ServiceBoundary`, `src/boundary.rs`), verbatim |
| `boundary_note` | string | hosted provenance, replay limits or the reason enrichment is unavailable, with the counts the scan kept. Always present. |
| `boundary_lines` | string[] | the boundary as the CLI prints it, line by line: `boundary_note` first, then the counts. A reader rendering the boundary prints these bytes rather than re-wording the struct, so a hook and a terminal say the same sentence about the same number. |
| `recheck` | object \| absent | what a `--recheck` call did (below). Absent on every other read, which means the items are the indexed ones — the answer this document described before carrick#1036 |
| `uses` | object \| absent | who uses this file from elsewhere in its repo, as indexed (below). Absent when the index predates the field; present with empty lists when the index recorded nothing for this file |
| `uses_lines` | string[] | `uses` as the CLI prints it, one line per type use and per used function. Always present; `[]` when there is nothing to say or the index predates `uses` |

### `recheck`

Present only when `--recheck` was passed AND the file had changed since the
index. Absent otherwise, including on a `--recheck` call for a file the tree
has not moved past: nothing needed re-judging, so nothing was.

| field | type | meaning |
|---|---|---|
| `ran` | string | `extraction+types`: the file was re-extracted, re-joined against the blobs the index holds, and at least one of its rows carries a type verdict. `extraction`: the same, and no row of this file carries one — nothing here pairs with anything, or its pairs were not both resolved, and each row's own `verdict.detail` says which. `none`: the items above are the indexed ones (when the edit changed nothing the scan reads, a comment or layout, no re-scan starts and each row's `line` is moved to where its node now sits) |
| `elapsed_ms` | int | wall time of the re-check, including one that missed its budget |
| `stale_since` | string (RFC 3339) | when the items above were computed. Present only on a `none` — otherwise the answer is now |
| `reason` | string | why the re-check did not run, in one sentence. Present only on a `none` |
| `new_functions` | array \| absent | functions this file declares in the working tree that the index does not hold under that name, as `{ "name": string, "line": int, "similar"?: array }`, in line order, at most 20. Named from a parse of the file that does not wait on the re-scan, so present on a `none` that missed its budget too. Absent when the edit added no function and when the index could not be read |
| `new_functions[].similar` | array \| absent | up to 6 indexed functions, in any repo of the index and outside this file, that this one may already exist as, best first, each `{ name, repo, file, line, end_line, score, signature?, exported, called_from, intent? }`. One ranking reads them: BM25 over a bag of words, the new function's name, body tokens and written signature against each indexed function's name, intent sentence, tokens and signature (`crates/carrick-match/src/similar.rs`). There is no score floor, so the list is empty only when no indexed function shares a word with the new one. A function that calls the other, or is called by it, is never listed, and the member named like the new function comes first. `called_from` counts the other files whose indexed calls name the row; `signature` and `intent` are the index's own and are absent when it holds none. `score` is for logs, never for a reader's text. Read from the index on this machine, with no network call |
| `known_miss` | `true` \| absent | on a `none` that did not start a re-scan, because a re-check of this file's service already missed its budget against this index. Absent otherwise |
| `deferred` | `true` \| absent | on a `none` from `--recheck --no-rescan` for an edit that needs a re-scan: the call was asked not to start one, so the rows are the indexed ones. Absent otherwise |
| `deadline` | `true` \| absent | on a `none` whose re-check started and ran past its budget, so the rows are the indexed ones because time ran out. With it, as with `known_miss`, `check <file> --recheck` exits 2. Absent otherwise |
| `scope` | object \| absent | which services this re-check read: `reread` (the file's service first, then every service holding an operation that reads a type declared in the file), `not_reread` (`{ service, operations, reason }` for each other service, saying whose copy its verdicts used), and `stale` (the `not_reread` services judged at a version that predates the work: edited since the index with no copy of the last re-check to stand in, or reading a path edited since the index, which their `reason` names as `reads <path>, edited since the index`). The hook's headline is never "no contract problem" while `stale` is non-empty: it says what it re-read and names up to three of the stale services. Absent on a `none` that decided no scope |
| `moved` | array \| absent | rows outside the edited file whose state this edit moved (below). Absent when the edit moved nothing |
| `engine_ready` | boolean \| absent | whether the compiler engine was ready for this re-check's re-scan: `false` when it was not found or missed its ready budget. The hook writes it to its timing rows. Absent when no re-scan ran |

### `recheck.moved`

The rule is `carrick check --changed`'s (carrick#2998), applied to the rows of
the edited file's repo and of every repo holding a re-read service: a row
counts when the index held the same row at the same line with a resolved,
compatible pair and the re-join now mismatches it, or when the index matched a
call to a route and the re-join finds none, and a file of one of that call's
old producers changed since the index (edited, added, deleted or renamed
away), or the re-scan still reads that producer's route at the same line under
another path (a mount on its chain moved it, carrick#3412). Any other call
whose producers are all in unchanged files is re-scan drift: not listed, with
one `rescan-drift` line in the debug log (on stderr when
`CARRICK_RECHECK_DRIFT` is set). A mount edit that removes a router's routes,
rather than moving them, reads the same as drift and is not listed. Neither is
a call that a handler-less route table still documenting the old path keeps
matched. A route renamed in its own file
lists the callers it strands, wherever they are. The rows of the edited file
itself are `items`, not here.

| field | type | meaning |
|---|---|---|
| `file`, `line` | string, int | where the row is, relative to the workspace root |
| `role` | string | `route` or `call` |
| `method`, `path` | string | the operation |
| `before` | string | `compatible`; `matched` for a call that had a route; `not_judged` when the index held no resolved, compatible pair |
| `after` | string | `type_mismatch`, `method_mismatch` or `no_route` |
| `counterparts` | string[] | the other side as `service file:line`; empty on `no_route` |
| `reason` | string | the compiler's or the matcher's sentence |
| `not_judged_before` | bool | the index never judged the pair, so the edit may not be what moved it: listed, never called a break |
| `other_build` | bool \| absent | the index was written by another carrick build, so the verdict may have moved because the build changed, not because of the edit. Absent when false |

The post-edit hook prints up to three of them as problem lines after the
file's own, naming the row and its counterpart.

`new_functions` is a NAME comparison against everything the index holds for that
file — the blobs the last local scan wrote AND the hosted record of the same
repo, because neither is present on every install — and both halves of that
decide what it means. The commit is the one the last scan of that repo ran
on — the default branch, in the ordinary case — so a function written earlier on
the current branch is listed here too. And the index holds no hash of a body, so
a function that was renamed appears as new and one rewritten under its old name
does not. The synthetic `<module>` definition (this file's own top-level call
sites, carrick#965) is never listed: nobody wrote it.

The post-edit hook reads only each function's `similar` rows. The end-of-task reuse nudge that named these functions (carrick#1330) was removed in carrick#3436.

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

### `uses`

What other files of the same repo use from this one (carrick#2067), folded from
the blobs in `.carrick/repos/` when the index is built (`src/local_mode/uses.rs`).
Locations only, never a verdict, so `touch` carries it too. A re-check replaces
the items and leaves this block as indexed. Every `file` is relative to the
top-level `repo`, because a call edge and a type's declaration never leave a
repo; counterparts keep their own `repo`. Callers and type uses inside the
queried file itself are left out.

An absent `uses` and an empty one are different answers: absent means the
index was written before the field existed, and `{ "functions": [], "types": [] }`
means the index recorded nothing outside this file using it.

| field | type | meaning |
|---|---|---|
| `functions[]` | array | functions this file declares that a function in another file calls, in line order |
| `functions[].name` | string | `Class.member` where a call site qualifies it, the bare name otherwise |
| `functions[].line` | int | where the function starts; `0` when the scan recorded no line |
| `functions[].callers_total` | int | every distinct caller |
| `functions[].callers[]` | array | at most 20 of them, by file and line: `{ "service", "file", "line", "function" }`. `function` is `<module>` for a call at the top of a file |
| `types[]` | array | types this file declares that a route or call in another file reads, in line order |
| `types[].symbol`, `types[].line` | string, int | the declared type and its line |
| `types[].operations[]` | array | `{ "kind", "direction", "service", "method", "path", "file", "line", "counterparts" }`: the row whose `request` or `response` type it is, and that row's counterparts in the `items[].counterparts[]` shape |

A type use is recorded only where the scan stated the type's declaration
(`defined_in` on the type manifest). A type nested in another, and a call made
through a published package, are not recorded.

### `uses_lines`

```
- type Widget (line 3) is the response type of GET /api/v1/widgets/:encoded at 2 call(s): src/inventory.ts:9, src/inventory.ts:17. Producers: catalog-web app/routes/api.v1.widgets.$widgetId.ts:11
- CatalogClient.readWidget (line 12) has 2 caller(s) outside this file: src/inventory.ts:9, src/inventory.ts:17
```

Types first in line order, one line per operation; then functions, most callers
first. A location in another service is prefixed with that service. A line
names at most four locations and counts the rest. The Read hook
(`carrick hook post-read`) prints them, with the other side of each route or
call in the file, the first time a session reads a TypeScript file. The
post-edit hook prints none of them: it counts them in one line, once per file
per session, and leaves that out too for a file the Read hook already covered.

### What the post-edit hook prints

Only what the edit needs acted on, a few lines per file
(`renderPostToolUse` in `npm/carrick/src/render.ts`):

- A file the re-check answered from the working tree: one line counting its
  contract problems, then at most three of them, one line each.
- A file the re-check did not answer for (it missed its budget, or none ran):
  one line that sends the reader to `carrick check --changed`, and none of the
  index's verdicts, which describe the file before the edit.
- A deleted file: what the index still holds for it, and its problems.

### `items[]`

| field | type | meaning |
|---|---|---|
| `kind` | `"route"` \| `"call"` | a route this file serves, or a call this file makes |
| `method` | string | HTTP method, GraphQL kind, or socket direction |
| `path` | string | route path, GraphQL field, socket event, or pub/sub topic |
| `line` | int \| null | 1-based line, when the index recorded one |
| `col` | int \| null | 1-based column, when the index recorded one |
| `source` | `"fact"` \| `"candidate"` \| `"not_stated"` | `fact` = a deterministic pass stated it; `candidate` = a model answer (`resolution_source` `model`, or `inline_literal`: a model row whose path is a literal in the source) from the scan that built the index, or a replayed hosted one, or a request the code builds and does not send (`request_descriptor`: a function nothing in its service calls returns `{ url, method, ... }`; its target is stated, nothing is seen sending it, and no response is typed for it); `not_stated` = the row recorded no source. Local facts remain authoritative when a model answer contradicts them. |
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
with their locations), then `uses_lines` under `used outside this file, as
indexed:` when there are any, then the staleness line, then the boundary. Written for
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
hosted index and are absent here.
  catalog-web at 4b96017: 0 file(s) sent to the analyzer
```

## On-disk layout

`carrick index --workspace <dir>` writes `<dir>/.carrick/`:

| path | what |
|---|---|
| `.carrick/.gitignore` | `*` — the directory ignores itself, so no user file has to change |
| `.carrick/repos/*.json` | local per-service blobs, including this scan's model answers and eligible replayed hosted ones |
| `.carrick/hosted/snapshot.json` | authenticated hosted data, metadata and credential fingerprint; no bearer token |
| `.carrick/hosted/last-sync.json` | Schema `carrick.hosted-sync/0`. It records what the last `carrick refresh --hosted-only` that the cloud answered found. Per repo it holds the hosted commit and its scan time, this machine's copy and its scan time, and whether HEAD holds the hosted commit (and how many commits behind it is when it does not). The session-start line reads it to name the hosted index the MCP answers from and, at most once a day, to say HEAD keeps a copy back (`hostedLine` / `heldLine` in `npm/carrick/src/render.ts`). |
| `.carrick/index.json` | the joined read model `touch` and `check` answer from: every repo's absolute path, its services with their commits and boundaries, and per file the rows with their counterparts and verdicts |
| `.carrick/build-*/` | transient per-run blobs and join result, removed when the build finishes |
| `.carrick/scan-<id>.log` | everything a detached build printed: its banner, its per-service lines, its map, and any error. Kept after the scan ends — it is the record of a run nobody watched |
| `.carrick/scan-<id>.json` | what that build is doing NOW: phase, service, counts, pid, when it started, and `spend` once an upload has come back with a figure. Removed when the scan finishes; left with `"status": "failed"` when it fails, and left as-is when the process is killed, which is how `carrick status` can say a scan stopped part-way |
| `.carrick/jobs.json` | the analysis Carrick Cloud is doing for this workspace: one entry per repo `carrick index --dispatch` handed over, with the job id, the commit the prompts were built at, how many files it carries and when it was submitted. `carrick resume` reads it and removes each entry once its answers are collected; `carrick status` reads it to say what is in flight. Outlives the scan record beside it, because a job outlives a build |
| `.carrick/last-scan.json` | what the last scan reported: one entry per repo `carrick index` scanned, each carrying the cloud's `carrick.scan-spend/0` block. Written as each block lands, so a run killed part-way still leaves the record of the repos it uploaded. Beside the index rather than in it, because `carrick refresh` rebuilds the read model from scratch and a first run may be killed before there is one. Nothing the CLI prints reads from it |

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

## `carrick check --changed` — every changed file, with the tests

Implemented in `src/local_mode/changed.rs`. Takes no file (to name the files
yourself, see "Several files" below). For each repo of
the workspace it finds the merge base of `HEAD` and the main branch
(`origin/HEAD`, else `origin/main`, `origin/master`, `main`, `master`), and
checks every source file the scan reads that differs from it: committed on the
branch, uncommitted, or untracked. Each repo with such a file is scanned once
with `--changed-only`, with no model. It asks Carrick's library store, read-only,
for the installed packages (names, versions, import specifiers and the TypeScript library globals the code uses, no source
code): only the services
whose inputs changed are re-extracted, and every other service is carried from
the blob the index holds for it (carrick#2826). The re-extracted services run
side by side: each one's type sidecar is held to a 3 GB heap (unless
`CARRICK_SIDECAR_MAX_OLD_SPACE_MB` says otherwise), and the scan runs as many
at once as the free memory holds at that cap. One join then runs over those
blobs and every blob the index already holds — the same two phases as
`check --recheck`, once for all files. A row in an untouched file
is reported only in the two cases under "Files the work did not touch" below.

Human output is one summary line, then one line per problem:

```
carrick check: checked 1 route and 2 calls in 2 changed files against 2 services: 1 incompatible, 1 reaches no route
catalog-web/app/routes/api.v1.widgets.$widgetId.ts:10  incompatible  route GET /api/v1/widgets/:widgetId with inventory-svc src/client.ts:20: Property 'activeCount' is missing ...
inventory-svc/src/client.ts:13  no route  call GET /api/v1/gadgets/:gadgetId: no service in this workspace serves it
```

`--json` prints `carrick.check-changed/0`: `files`, `routes`, `calls`,
`services`, `repos` (each with `main_branch`, `base`, `files`, `rescan`, or `skipped`),
`problems` (`kind` `incompatible` | `no_route`, `file`, `line`, `role`,
`method`, `path`, `other`, `reason`, `source`, `already_indexed`,
`unconfirmed`, `outside_the_change`, `other_build`, `fails`), `failing`,
`outside_moved`,
`elapsed_ms`, `not_checked` when nothing could be checked, and
`index_scanner` when the index was written by another scanner build.

**Exit code.** The one read a verdict moves: 1 when any problem `fails`, else
0. An incompatible pair fails when its finding's edge source is `fact` and its
verdict state is not `unresolved` (the cloud's enforcement rule, cloud#599); a
call that reaches no route fails when its row is a fact. Either one fails only
when it is new since the index: the same operation in the same file did not
already carry it (`already_indexed`), so a file the work touched never fails
on what was there before. A call whose base is set at run time (an injected
base: a parameter, a field, a config value that reads no environment variable)
is `unconfirmed` and never fails, because the check cannot tell a service of
this workspace from an outside API there; the exception is a call the index
matched to a workspace route, whose loss is a fact. A problem resting on a
model candidate, or on rows that state no source, exits 0 too.

**An index another scanner build wrote** (carrick#2911). The index records
the version and the stamped commit of the build that wrote it. When that is
not the running build (after an upgrade, or a local build), the index's
silence about a row is no evidence the row is new: that build may not have
read it. A problem that is new only by that silence is `other_build`: it
is listed without `--all`, marked `[not checked: the index is from another
scanner build]`, does not fail, and the run exits 2 when nothing else fails,
with one line naming both builds and saying to run `carrick refresh` on the
main branch (on a branch, `refresh` would take the branch's own new problems
in as already indexed). What the index did read still decides: a call it
matched that now reaches no route fails whichever build wrote it.

**A library store that cannot be asked** (carrick#3249). The re-scan asks
Carrick's library store which exports of the installed packages register routes
and make calls. When the ask gets no answer, every row a library package states
is missing: a route registered through a server package does not exist locally,
a pass over it would read "checked 0 routes", and every caller of it would read
as a call to no route. The check does not pass and does not fail on those
claims. It prints one line and exits 2. With no credential, nothing is sent:

```
carrick check: not checked: library facts unavailable: this machine is not signed in. Run carrick login.
```

With a credential and a store that cannot be reached (a network error, a
timeout, a 5xx):

```
carrick check: not checked: library facts unavailable: Carrick's library store could not be reached.
```

`--json` carries the same sentence as `not_checked`. `check <file> --recheck`
reports it as `recheck.ran` `none` with it as `recheck.reason`, and exits 2; so does an index commit the clone does not hold (its "routes and calls are missing" sentence, carrick#3413). A
store that answered, including with no claims for a package, and a signed-in
run that reaches the store, print and exit as before. The CI runner is
untouched: it signs in with OIDC, and a local-mode check does not run there.
The mark is read from the scan of the services re-extracted for the check, and
a service carried from an index written while offline keeps its mark until the
next `carrick refresh`.

**Pairs the check could not judge** (carrick#3012). A matched route or call in
a changed file whose pair has no verdict, or whose verdict is unresolved
because one side has no usable type, cannot show a type change. It is counted
on the summary line as `not type-checked`, listed with `--all` and in the JSON
as `not_type_checked` (`file`, `line`, `role`, `method`, `path`, `reason`),
and never counted as a pass. When the changed files hold such pairs, none was
type-checked (`type_checked` is 0) and nothing fails, the run exits 2. A run
that judged some pairs exits by those. A pair whose calls are all built
requests (`request_descriptor`) is still counted and listed as not
type-checked, but does not make the run exit 2: its matched method and path
are the check it can have. That holds whichever side changed, so a branch
that only adds request builders exits 0, and so does one that only changes a
route those builders alone call (carrick#3532).

A check that
covered none of the files it was asked about exits 2, which no verdict uses:
no index, an index not bound to this folder, a file not in the index, a
service that is not indexed, or the budget spent. It names each file or repo
and the reason, and says to run `carrick init` or `carrick refresh`. A check
that covered some files exits 0 or 1 by those files and lists the skipped ones
as `not checked:` lines. `--changed` with no changed file asked about nothing
and exits 0. `carrick hook post-edit` does not run this pass and keeps its
exit codes.
The budget is five minutes, `CARRICK_CHECK_BUDGET_MS`.

**Nothing the scan reads moved.** A repo's `rescan` is `not_needed` when every
file that differs from the commit the index was read at (clean tree, one
commit for the whole repo) either is not read by the scan's input rule or is a
source file whose two copies parse, are equal ignoring positions, and hold the
same `/** */` blocks, `@ts-` / `@jsx` comments and `///` lines in the same
places. The check then starts no re-scan, says `nothing the scan reads
changed in <repo>` on stderr, joins the index's own blobs, and moves each
reported line in those files to where its code now sits. A manifest, lockfile,
config, test file, added or deleted file, parse failure or missing base commit
makes it `ran`, as does `CARRICK_CHECK_RESCAN=always`, a support switch that
forces the re-scan. `rescan` is absent for a repo with no changed file to
judge.

**Quiet by default.** Only problems that fail the run are listed. The rest
are counted on the summary line by why they do not fail, `..., plus 2 already
in the index and 1 unconfirmed, run with --all to list them`, and `--all`
lists them, each marked with the same reason: `[already in the index, does
not fail]`, `[unconfirmed: the base is set at run time, does not fail]`,
`[model candidate, does not fail]`, `[built request not seen sent, does not
fail]` (a `request_descriptor` call, counted as `1 built request not seen
sent`) or `[source not stated, does not fail]`.
A problem the index already held is counted as that, whatever else holds for
it. `--json` always lists every problem, with its `source`,
`already_indexed`, `unconfirmed` and `fails`.

**Files the work did not touch.** A row in such a file, in a repo this run
re-scanned, is reported in two cases. Both are marked `outside_the_change` and
never add to `routes`, `calls` or `type_checked`, which are about the changed
files.

- A call the index matched, whose old producer sits in a file changed since
  the index or is still read at the same line under another path (a mount on
  its chain moved it), that now reaches no route. It is listed when it
  fails, and with `--all` otherwise. A mount edit that removes the routes,
  rather than moving them, reads as re-scan drift and is not listed.
- A pair whose rebuilt verdict is a mismatch (`type_mismatch` or
  `method_mismatch`) and that the index did not already hold as a mismatch at
  the same line. If the index held it resolved and compatible, the work moved
  it: it fails like a problem in a changed file, on a fact. If the index held
  no such row, or an unresolved one, it may have resolved for a reason
  unrelated to the edit, so it never fails: `--all` lists it, and the summary
  counts it as "not judged before". `outside_moved` and the summary
  line's "moved" clause count only the first kind, the pairs the index held
  compatible.

A pair the work moved is listed on a plain run even when it does not fail (a
model candidate, for one), with its usual note such as `[model candidate, does
not fail]`, and the summary counts it as `--all` does, under "do not fail the
check". Up to 10 are listed; the rest end in `and N more: carrick check
--changed --all lists them`. Exit codes and `--json` are unchanged. Every
count on the summary line is either backed by listed rows or followed by
`run with --all to list them`; the "not type-checked" count carries that
pointer too.

**Several files.** `carrick check a.ts b.ts [--all] [--json]` runs the same
pass over the files named, in any repos of the workspace, instead of the
files git says changed: always re-extracted (`--recheck` is accepted and
changes nothing), each repo scanned once, one join, the same summary line,
output and exit code. A file outside the workspace, or one the scan does not
read, is listed as `not checked: <file>: <reason>` and the others are still
checked. One file keeps the single-file read above.

## `carrick status` — the workspace, with no file in the question

What a surface opening a session asks: what is indexed, at which commit, how
far each repo has moved since, and what each service could not classify. Same
rules as the other reads: local index and credential identity, under 300 ms,
and exit 0 whatever it finds — including a refusal, unlike `check`.

Its refusal is written for a command that takes no file, and it never names the
scan that is already running: a workspace whose first scan is in flight is
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
| `last_scan` | what the last scan reported: `updated_at`, and `scans[]` of `{ repo, spend }` where `spend` is the cloud's `carrick.scan-spend/0` block (`usd`, `priced`, `unpriced_models`, token counts, `first_index_ceiling_usd` / `first_index_remaining_usd`, `monthly_allowance_usd` / `monthly_remaining_usd`, `period`). Absent until a scan has run, and carried on the error body too, because a run killed before it wrote an index still uploaded the repos it got through. Read by whoever parses the JSON; the CLI itself prints none of it |
| `analysing[]` | one line per repo whose analysis Carrick Cloud is doing right now, and the only part of any read command that touches the network — present only when `.carrick/jobs.json` names a job. Says how far it has got, that it is ready and nothing has collected it, or that this machine could not ask. Carried on the error body too: "no index" and "the analysis that builds it is running elsewhere" are different answers |
| `services[].boundary.candidates_awaiting_model` | candidates no model has been asked about, so `0 route(s) 0 call(s)` from a free pass is distinguishable from a service with no API in it. Zero after a scan that ran the model; absent on an index written before the count existed |
| `services[].boundary.time_limits_run_out` | what the scan's time limits cost this service when they ran out: `retype` (response checks not reached), `library_checks` (library checks not reached), `sidecar_deadline` (type lookups the type checker never answered; counted here and in the run log, never printed). `{}` when none ran out; absent on an index written before the count existed |
| `services[].boundary_lines` | the same pre-rendered lines `check` and `touch` carry |
| `services[].hosted`, `services[].hosted_state` | the same provenance and replay state as `check` |
| `hosted_checked_at` | when repository metadata last answered for this workspace |
| `repos_detected_by` | `carrick_json`, `workspace_manifest`, `siblings`, `single_repo`, or `workspace_overrides` |
| `repos_added`, `repos_excluded` | explicit workspace override paths; absent when empty |
| `type_check` | what the last build's type check did: `pairs` matched, `judged` of them, `skipped` with the reason when it did not run, and `not_stored[]` of `{ consumer, producer, verdicts, indexed_before_producer, check_skipped }` for verdicts judged here that a consumer's upload does not carry, where `check_skipped` is why that consumer's own scan did not run its check. A laptop scan pairs with the siblings indexed before it, so a consumer scanned before its producer uploads without that verdict. The human `status` and the build's own output print it as lines. Absent on an index written before it existed |

Errors are the same three, with the same `message` beside the code, under this
schema: `{ "schema": "carrick.status/0", "error": "not_indexed", "message": "..." }`.

## Out of scope in this version

- Local model calls.
- Per-file incremental extraction and a persistent process.
- Background hosted refresh timers or changes to the MCP tool surface.
