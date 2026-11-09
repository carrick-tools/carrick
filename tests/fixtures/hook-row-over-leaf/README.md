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
| 7 | none: the model's row is withdrawn |
| 9 | `POST /api/v1/boards/:board/feeds/:feed/refresh`, `request_summary`, type `{ data: unknown; }` |
| 17 | none: the model's row is withdrawn |
| 19 | `POST /api/v1/boards/:board/feeds/:feed/archive`, `request_summary` |

Before the fix, line 7 and line 9 both held a row for the refresh request,
and line 17 held the only row for the archive request: the model's row there
kept the call inside from stating one. Either way a row stood at the hook
call, typed as the hook's state.

Test: `tests/hook_row_over_leaf_test.rs`.
