# Address chain

A route row in the index can carry its **address**: the ordered list of
statements its path is composed of, outermost first, ending with the row's
own call. Each statement is a **part**, with the `file:line` that states it
and the value it states. The types are in `src/address.rs`; the scanner
records them while it composes a route's full path from the library store's
mount and scope labels (`src/request_summary/decided_sites/composed.rs`).

The address is a pipeline fact, not a rule. The scanner reads it from the
same calls that compose `full_path`; nothing in it names a framework.

## Where it is stored

`mount_graph.endpoints[].address` in the index blob. The field is optional:

- **absent** means this scanner recorded no address for the row: the blob
  was written before addresses existed, or the row is of a kind no slice
  records yet (see "What is recorded" below). It never means "a route with
  no chain".
- **present** means every part was recorded. A part the scanner cannot read
  is stored as an `unknown` piece with its reason; it is never dropped.

`full_path` stays the readable text and is unchanged by the address.

The cache version did not change when the field was added: the address is
derived on every scan from the calls, and the cache holds only the model's
answers. A row the trace writes on a scan carries its address on that scan.

## Shape

```json
{
  "parts": [
    {
      "kind": "mount",
      "dimension": "path",
      "at": "src/server.ts:13",
      "member": "mount",
      "value": {
        "value": "text",
        "values": [[{ "piece": "folded", "text": "/api", "from": "src/config.ts" }]]
      },
      "via": "label"
    },
    {
      "kind": "own",
      "dimension": "path",
      "at": "src/server.ts:8",
      "member": "get",
      "value": { "value": "text", "values": [[{ "piece": "lit", "text": "/orders/:id" }]] },
      "via": "label"
    }
  ],
  "reading": "whole"
}
```

### Part

| Field | Meaning |
|---|---|
| `kind` | `mount` (a member that serves another instance's routes under its prefix), `scope` (a member that opens a scope its routes are served under), `own` (the row's own call) |
| `dimension` | Which axis of the operation key the part extends: `path` |
| `at` | Repo-relative `file:line` of the call that states the part |
| `member` | The member as written (`mount`, `register`, `get`). Display only; absent when none |
| `value` | `{"value": "text", "values": [...]}`, one entry per value the statement states, each a list of pieces; or `{"value": "unstated"}` when the member writes nothing there |
| `via` | `label`: read from a call the library store's verified label names |

### Piece

| `piece` | Fields | Meaning |
|---|---|---|
| `lit` | `text` | Text written at the part's own call |
| `folded` | `text`, `from` | A constant the scanner folded to text; `from` is the repo-relative file that declares it |
| `unknown` | `written`, `reason`, `reads` | A value not read as text, as written. `reason` is `runtime_env` (an environment read; `reads` names the variable), `parameter`, `conditional` or `unread` |

### Reading

Derived from the parts and stored so readers need not re-derive it:

| `reading` | When |
|---|---|
| `whole` | Every piece is `lit` or `folded` |
| `headless` | Every `unknown` piece comes before the first read piece |
| `gap` | A whole segment after a read piece is `unknown` |
| `unread` | The `own` part holds an `unknown` piece, or one sits inside a segment |

## What is recorded

- HTTP routes the receiver trace serves from the top (`library_claim` rows
  the trace writes, corrects or confirms). Every such row reads `whole`:
  a route under an unread prefix is not written as a row yet.
- A mount or scope prefix that is an imported constant of the service is
  stored `folded` with its declaring file. A value read through a
  parameter's callers is stored `lit` without `from`.
- The own part is stored `lit`, as the path the call writes.
- When two chains compose the same full path, the row stores the first.

Not recorded yet (the field is absent): procedure (RPC router) rows, rows the
cross-service join writes, socket, GraphQL transport and pub/sub rows, rows
from the model, documentation rows, and decorator routes.
