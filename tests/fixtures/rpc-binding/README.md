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

## Consumer rows (S2), declared service

Each call of a procedure's variable is a request row at the call, keyed by
its procedure's own key (copied from the producer, never read again), with
`path` `""` and the method its definition states (read once, at the
definition's site):

| File | Row |
|---|---|
| `page.tsx:6` | `POST ""` at the `save(` call, pairs `saveNote` |
| `handed.ts:8` | `POST ""` at the `keep(` call, pairs `saveNote`: handing the alias on as well does not stop it |
| `direct.ts:4` | `GET ""` at the `listNotes()` call, pairs `listNotes` |
| `value.ts:9` | none: the alias is handed on and never called here; the `useFn(` call is saved `detached_unplaced` |
| `control.ts` | none: a plain local function, no row and no saved site |

The `useFn(` calls whose alias is called state no site of their own. Three
pairs within the one service (two keys: `GET` and `POST`), one orphan
(`unused`). Without the pass-through scopes `saveNote`'s method is unread,
so its two calls are saved `unread`, never stated on a guessed method.
Request types `{ body: string }` and `undefined`; response types
`{ ok: boolean }` and `{ id: number }[]` (T1).

The wrapper's request op (`useFn`, `member: null`, `name: { bound:
"maker" }`) is read from the store's claim with no scanner verdict, as every
op on an instance is today (carrick#4847): the direct verifier answers it
`unchecked member_untyped`. When carrick#4847 requires a verdict, that op
must verify, or the `page.tsx` and `handed.ts` rows stop.

Gaps: a procedure defined in a sibling service is not read (the procedure
set is this service's); its calls state nothing.

## Where it is read

- The procedure set is the decided sites `Pass::bound` keys
  (`decided_sites/procedures.rs`); its calls are read against it by
  `Pass::binding_calls`, matched on where the compiler declares the called
  value and where `router_holder::holder_of_call` says the key's name is
  written.
- The analyzer keys a matched, consumed and orphaned producer with its
  procedure (`analyzer/mod.rs` `producer_operation_key`), so one paired
  `POST ""` procedure never marks another consumed.
