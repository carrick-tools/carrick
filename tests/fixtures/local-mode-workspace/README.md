# local-mode-workspace — a two-repo workspace for the local read-only path

Two repos and one contract between them, built so **every row is deterministic**:
the local index (`carrick index`) runs with no model at all, so a fixture whose
rows come from extraction would hold nothing here. See
`tests/local_mode_test.rs`, which is the only consumer.

- `catalog-web/` — the producer. A flat-route module derives
  `GET /api/v1/widgets/:widgetId` from the file name plus its exported `loader`
  (source: `file_based_route`). Nothing about it needs a model.
- `inventory-svc/` — the consumer. A client class states the same path and verb
  at a member the caller imports (source: `imported_member`).

They match on `http|GET|/api/v1/widgets/:widgetId`, which is what makes
`carrick touch` on either side name the other.

## What the test edits, and why the edits are what they are

The test copies this tree to a temp dir before touching anything — the fixture
on disk is never mutated.

| edit | what it does to the contract |
|---|---|
| `export const loader` -> `export const action` in the route module | the same path is now served by POST, so the consumer's GET is a **method mismatch**. A break with no dependency install and no model: the route's verb comes from the exported handler's name. |
| a comment added to the route module | additive: the contract is unchanged, and `check` must stay quiet. |
| the route module deleted, without re-indexing | the index still serves the route from a file that is gone: **producer removed**, with its consumers listed. |

Answer keys elsewhere in `tests/fixtures/` are spec-of-record and are never
edited to make a run pass. This one is a fixture for a CLI, not a scored
corpus: the table above IS its answer key, and a change to the tree changes the
table in the same commit.
