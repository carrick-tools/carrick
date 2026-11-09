# hook-row-over-leaf

A model row at a hook call against the row the call inside its callback
states. Two services:

- `packages/client` writes two requests as specs (`transport.post({ url })`).
- `apps/web` reaches them from `useMutation` callbacks
  (`apps/web/src/feed-hooks.ts`) through the app's own functions
  (`apps/web/src/feed-api.ts`). Each hook has a second callback calling a
  name nothing resolves, so the hook row itself is left as the model stated
  it (carrick#2832).

The cassette under `__llm__/` answers at each `useMutation(` call with the
request the callback makes: once with the path's holes named its own way
(line 7), once named as the client does (line 17).

Answer key for `apps/web/src/feed-hooks.ts`:

| Line | Row |
|---|---|
| 7 | none: the model's row is withdrawn, the leaf row at line 9 states its request |
| 9 | `POST /api/v1/boards/:board/feeds/:feed/refresh`, `request_summary`, type `{ data: unknown; }` |
| 17 | `POST /api/v1/boards/:board/feeds/:feed/archive`, `model`: recorded as it is today, pending carrick#3640 |
| 19 | none today: the model's row at line 17 states the same operation exactly, so it holds the leaf (the rule `call-through-client`'s `nested.ts:7` answer key pins) |

Before the fix, line 7 and line 9 both held a row for the refresh request,
and the type check judged the response against the hook's state at line 7.

Test: `tests/hook_row_over_leaf_test.rs`.
