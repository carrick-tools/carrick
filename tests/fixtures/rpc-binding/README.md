# rpc-binding

Answer key for procedures named by the variable that holds them (contract
amendment 13, `name: { bound: "binding" }`; carrick#4909, carrick#4919). The
packages are invented: `@fixture/rpc-core` declares the maker `defineFn` and
its builder interfaces, and `@fixture/rpc` re-exports `defineFn` and declares
the wrapper hook `useFn`.

Two services hold the same source:

| Service | Declares | `defineFn` imported from | Store answer |
|---|---|---|---|
| `services/declared` | `@fixture/rpc`, `@fixture/rpc-core` | `@fixture/rpc-core` | `library-claims-declared.json` |
| `services/reexport` | `@fixture/rpc` only | `@fixture/rpc` (a re-export) | `library-claims.json` |

The re-export service states nothing until carrick#4882 (carrick#4878) lands:
the call's declaration is in `@fixture/rpc-core`, which the store did not
answer, so no claim is asked. Its test is ignored until then.

Both store answers carry the D3 op on `defineFn`, the D4 make and op on
`useFn`, and two pass-through scopes on `defineFn`'s instance (`use`,
`validate`, `passes: true`). Without them the instance walk stops at a
chained call, so `saveNote`'s op call never reaches its maker call and its
method (`maker: true`) is unread: no row is written at line 5, the model's
row stands, and the definition is saved as an unread site with reason
`unread` (the instance walk's own reason; the control test strips them).

`services/declared/src/grouped.ts` holds two definitions under one
declaration: they would share one key, so neither states a row, and both
(lines 4 and 5) are saved as unread sites with `procedure_router_ambiguous`.

`__llm__/analyze-file/notes.functions.json` is the model's answer for
`notes.functions.ts`: one row, `POST /saveNote` at line 5, a path the app
does not serve.

## Producer rows (S1), per service

The library serves each procedure at a path it chooses when the app is built,
so no path is stated: `path` and `full_path` are `""`. Each row is keyed by
its procedure, `{path: <variable>, router: {file: <defining file>, binding:
""}}` (the declaration around the variable; `""` at the module's top level),
and sits on the line its `defineFn(` call starts on.

| Line | Method | Procedure | Why |
|---|---|---|---|
| 3 | GET | `listNotes` | `defineFn({ method: "GET" })`; no model row there, so written |
| 5 | POST | `saveNote` | `defineFn({ method: "POST" })`; replaces the model's `POST /saveNote` |
| 10 | GET | `unused` | `defineFn()` writes no method: the label's `method_default`; no caller |

Three rows after the cross-service merge, not two: the merge key carries the
procedure.

## Consumer rows (S2, not yet asserted)

| File | Row |
|---|---|
| `page.tsx` | at the `save(` call, pairs `saveNote` |
| `direct.ts` | at the `listNotes()` call, pairs `listNotes` |
| `value.ts` | none: the alias is handed on and never called here; saved with a reason |
| `control.ts` | none: a plain local function |

Two pairs, one orphan (`unused`). Request types `{ body: string }` and
`undefined`; response types `{ ok: boolean }` and `{ id: number }[]` (T1).

## For S2

- The analyzer's matched and orphan keys carry no procedure: `analyzer/mod.rs`
  keys a producer as `method:full_path` plus the dispatch case, where pairing
  inserts its winners (`matched_endpoints`, about line 2941), in the consumed
  key (about line 3153) and in the orphan lookup (about line 3391). Every
  `POST ""` procedure of a service therefore shares one key: one paired
  procedure marks the others consumed, and their orphans go unreported.
  The procedure belongs on those keys, as `procedure_suffix` is on the
  merge key (`mount_graph.rs` `endpoint_merge_key`).
- The run's procedure set is the decided sites: read `DecidedSite.procedure`
  where `binding` is true. Its `at` holds the key path, the file and the
  router binding; the wire method is the site record's (the Method slot),
  read once there.
