# summary-method-from-label

Answer key for `tests/summary_method_from_label_test.rs` (carrick#4373 T1).

`@fixture/http` is a package the library store labels an HTTP client
(`__llm__/library-claims/default.json`): `post` sends POST, `remove` and
`purge` always send DELETE, and `request` reads its method from the `method`
key of its second argument, GET when nothing is written there.
`@fixture/bus` is a pub/sub broker (`publish`), `@fixture/wire` a socket
client (`emit`), and `@fixture/legacy` a package the store does not label.
`package-lock.json` must stay: without it the store is never asked.

Each `remove`/`purge` call passes `{ headers: {} }`, an options bag that
states no method. The request summaries would read that bag as GET; the
store's label says DELETE, and the label decides.

| file:line | Shape | Right |
|---|---|---|
| `src/counter.ts:7` | same-file wrapper around `remove` | DELETE |
| `src/purge.ts:7` | same-file wrapper around `purge` | DELETE |
| `src/xclear-caller.ts:3` | cross-file wrapper (`src/lib/xclear.ts`) | DELETE |
| `src/retawait.ts:6` | `return await remove(...)` | DELETE |
| `src/query.ts:7` | path with a query hole | DELETE |
| `src/fwd.ts:7` | caller forwards its own `id` | DELETE |
| `src/xdrop-caller.ts:3` | cross-file, forwards `id` | DELETE |
| `src/pair-lines.ts:7` | `remove` and `post` in one caller | DELETE + POST |
| `src/jobs.ts:7` | `request`, nothing written at `method` | GET |
| `src/jobs.ts:8` | `request`, the caller writes `"POST"` | POST |
| `src/poster.ts:7` | `post` | POST |
| `src/fetcher.ts:5` | platform `fetch` | GET |
| `src/legacy.ts:7` | unlabelled package | GET |

The message sends (`publish*.ts`, `emit.ts`, `x*-caller.ts` around `bus` and
`wire`) belong to T3 of carrick#4373; here they must not change.
