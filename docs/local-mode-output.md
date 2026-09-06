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
| `carrick index --workspace <dir>` | the repos listed in `<dir>/carrick-workspace.json` | `<dir>/.carrick/` | minutes, cold |
| `carrick touch <file> [--json]` | `.carrick/` only | nothing | < 300 ms |
| `carrick check <file> [--json]` | `.carrick/` only | nothing | < 300 ms |
| `carrick refresh [--service <name>]` | one service's source | `<dir>/.carrick/` | seconds |

`touch` and `check` never parse the file, never call a model, never call the
cloud, and never re-extract. They read what `index` already computed. `touch`
answers "what is on the other side of what I am editing"; `check` adds the
contract verdicts computed at index time. Both exit 0 whatever they find:
local mode is advisory, nothing blocks.

`index` computes deterministic facts only. There is no model on the laptop, so
no candidate is classified locally and the boundary block says so.

## JSON

```json
{
  "schema": "carrick.check/0",
  "file": "app/routes/orders.$id.ts",
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
          "file": "src/api/orders.ts",
          "line": 44
        }
      ],
      "verdict": {
        "state": "resolved",
        "result": "compatible",
        "detail": "request and response types compared by the compiler"
      }
    }
  ],
  "boundary": {
    "commit_hash": "a1b2c3d4e5f6",
    "files_attempted": 0,
    "candidates_not_classified": 41,
    "unemitted_literal_candidates": 12
  }
}
```

### Top level

| field | type | meaning |
|---|---|---|
| `schema` | string | always `carrick.check/0` for this document |
| `file` | string | the queried file, relative to the repo that owns it |
| `service` | string | the service the file belongs to (`serviceName` from `carrick.json`, else the repo name) |
| `index_commit` | string | the commit that service was indexed at |
| `indexed_at` | string (RFC 3339) | when `index` (or the last `refresh` of this service) ran |
| `scanner_version` | string | the scanner release that wrote the index |
| `changed_since_index` | int | files changed since `index_commit`: `git diff --name-only <commit>` plus files whose mtime is newer than the index |
| `stale` | bool | this file is one of them, so its rows may not describe what is on disk now |
| `deleted` | bool | the file is in the index and no longer on disk |
| `items` | array | routes and calls the index holds for this file, in line order |
| `boundary` | object | what this service's scan could not classify (`ServiceBoundary`, `src/boundary.rs`) |

Locations come first and the boundary comes last: a reader that stops early has
read the facts, and a reader that reads to the end knows what is missing.

### `items[]`

| field | type | meaning |
|---|---|---|
| `kind` | `"route"` \| `"call"` | a route this file serves, or a call this file makes |
| `method` | string | HTTP method, GraphQL kind, or socket direction |
| `path` | string | route path, GraphQL field, socket event, or pub/sub topic |
| `line` | int \| null | 1-based line, when the index recorded one |
| `col` | int \| null | 1-based column, when the index recorded one |
| `source` | `"fact"` \| `"candidate"` | `fact` = a deterministic pass stated it; `candidate` = the model's reading alone. Local mode indexes no model rows, so a locally-produced row is always `fact`. |
| `resolution_source` | string \| null | the wire value from the index blob: `file_based_route`, `imported_member`, `model`, … `null` = this row does not state it |
| `evidence` | string \| null | one line naming what the row was read off |
| `counterparts` | array | the other side of the contract, across every repo in the workspace |
| `verdict` | object \| null | `null` from `touch` always; `check` fills it in |

### `items[].counterparts[]`

| field | type | meaning |
|---|---|---|
| `role` | `"producer"` \| `"consumer"` \| `"peer"` | what the counterpart is. `peer` is a shared external contract: both sides call the same third party, and neither serves the other. |
| `service` | string | the counterpart's service |
| `file` | string | the counterpart's file, relative to its own repo |
| `line` | int \| null | 1-based line, when the index recorded one |

### `items[].verdict`

`null` from `touch`, and from `check` on a row nothing was compared for.

| field | type | meaning |
|---|---|---|
| `state` | `"resolved"` \| `"unresolved"` \| `"not_checked"` | `resolved` = the index holds a verdict; `unresolved` = the file changed since the index, so the verdict describes an older tree; `not_checked` = matched, never compared |
| `result` | `"compatible"` \| `"type_mismatch"` \| `"method_mismatch"` \| `"producer_removed"` \| null | null while `state` is `not_checked` |
| `detail` | string | one line, written to be read by a model |

`producer_removed` is the deleted-route case: the file is gone from disk, the
index still holds the route, and its consumers are listed as counterparts.

### Errors

Exit code is 0 for every read-only command, including failure: a hook must
never fail an edit. A missing or unreadable index prints one line to stderr and
this to stdout:

```json
{ "schema": "carrick.check/0", "error": "not_indexed" }
```

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
app/routes/orders.$id.ts (webapp, indexed at a1b2c3d)

  route  GET /api/orders/:id  line 12  [fact: file_based_route]
    consumer  admin-ui  src/api/orders.ts:44
    verdict   compatible (compiler-compared)

changed since index: 3 files; this file has changed since it was indexed.
boundary (webapp): 41 candidates not classified locally, 12 unemitted literal
candidates. Candidates are not classified locally: no model runs on this
machine.
```

## On-disk layout

`carrick index --workspace <dir>` writes `<dir>/.carrick/`:

| path | what |
|---|---|
| `.carrick/.gitignore` | `*` — the directory ignores itself, so no user file has to change |
| `.carrick/workspace.json` | the resolved workspace: every repo path, its service ids, the commit each was indexed at, the scanner version, the index time |
| `.carrick/repos/<repo>__<service>.json` | the per-service index blob (`CloudRepoData`), written by `LocalDirStorage` — the same bytes the cloud path would upload |
| `.carrick/index.json` | the joined read model `touch` and `check` answer from: items per file, counterparts, verdicts, per-service boundary |

`index.json` is derived: deleting it and re-running `carrick index` reproduces
it. Nothing outside `src/local_mode/` reads it, and its internal shape is not
this contract — only the command output above is.

`<dir>/carrick-workspace.json` is the input, written by hand or by `carrick
init`:

```json
{ "repos": ["./webapp", "./orders-service", "../shared-client"] }
```

Explicit paths, relative to the workspace file. No directory walk.

## Out of scope in this version

- **Pulling candidates from the cloud index.** The scanner authenticates with
  GitHub OIDC, which a laptop does not have, so local mode reads disk only. The
  model rows the cloud index holds for these repos are not merged in. That
  needs a user-auth path and is not built.
- Per-file incremental extraction, a persistent process, and socket/pub-sub
  edges in the local read path.
