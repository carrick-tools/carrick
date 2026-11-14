# site-maker-response

Answer key for carrick#4319: where a decided site's response is read when
the deciding call is not the labelled call.

`@fixture/http` (in `node_modules/`) declares `post<T>`, `remove<T>` and
`purge<T>`. The store's claims (`__llm__/library-claims/default.json`) label
`post` as POST with its body at argument 1, `remove` as DELETE with its body
at argument 1's `data` key, and `purge` as DELETE with no body slot. The
reader's cassette (`__llm__/analyze-file/`) states one row at each labelled
call.

Two shapes put the deciding call above the labelled call:

- **A maker.** The deciding call makes the function that sends and does not
  run it (`makeGate({ base: "/svc" })` returns a closure that calls
  `http.remove`). Its own result is that function, so the site's response is
  what the request answers, read at the labelled call.
- **A wrapper.** The deciding call runs the send and hands its result back
  (`dropJson<{ rows: number }>("/ledger")`). The response is read at the
  deciding call, where a type parameter the caller fills is closed.

## Site rows: response (T1)

| Row | Shape | Wanted response |
|---|---|---|
| `gate.ts:7` | maker | `{ ok: boolean; }` |
| `poster.ts:7` | maker; the closure takes the body | `{ id: string; }`; request stays `{ sku: string; qty: number; }` |
| `twice.ts:6`, `:7` | one maker, two calls | `{ up: boolean; }` each |
| `partial.ts:6` | maker; `:7` passes an unbound parameter | `{ swept: number; }` |
| `handed.ts:6` | maker also handed on as a value at `:8` | `{ flushed: boolean; }` |
| `pinger.ts:6`, `:7` | maker through `post`; old-path rows on the site's line | `{ up: boolean; }` each |
| `typed-maker.ts:6` | generic maker | `unknown`, root reason `bound_by_callers` (the maker's type parameter is open at the labelled call) |
| `env-maker.ts:6` | maker given an environment base | no entry (its key is not a bare path) |
| `generic.ts:6` | wrapper that runs the send | `{ rows: number; }` |
| `local-run.ts:7` | wrapper that runs a local closure | `{ done: boolean; }` |
| `retried.ts:8` | wrapper that hands a local closure to `retry` | `{ n: number; }` |
| `direct.ts:3` | the labelled call is the deciding call | `{ ok: boolean; }` |

Before T1 every maker row above read the closure's own type
(`(tag: string) => Promise<{ ok: boolean; }>`), and `typed-maker.ts:6` read
`() => Promise<{ count: number; }>`.

## Model rows at the labelled call (T2)

Where every deciding call of a labelled request only makes the function
that sends, holds the request's fact row, and keys it by a bare path, the
model's row at the labelled call is the same request stated again. It goes
from the calls, the graph and the manifest; its type is read at the site
rows (T1).

| Row | Model row | Why |
|---|---|---|
| `gate.ts:5` | goes | maker |
| `poster.ts:5` | goes | maker |
| `twice.ts:4` | goes | maker, both calls stated |
| `typed-maker.ts:4` | goes | maker |
| `partial.ts:4` | stays | `:7` leaves the name open (a lead) |
| `handed.ts:4` | stays | the maker is handed on as a value at `:8` |
| `env-maker.ts:4` | stays | the site's key has an environment base, so it has no manifest entry; the model row keeps `{ trimmed: number; }` |
| `generic.ts:4`, `local-run.ts:4`, `retried.ts:5` | stays | a wrapper that runs the send |
| `pinger.ts:4` | stays | no fact row was stated (old-path rows keep the site) |
