# site-wrapper-promise-class

Answer key for carrick#4513 P2 (READY-4513-finish seed S1): a wrapper that
hands back a Promise class it builds around the request.

The platform `fetch` is labelled by the `lib:lib.dom.d.ts` claim
(`__llm__/library-claims/default.json`), and the reader's cassette states a
row at the `fetch` line in `src/seed-promise-class-lib.ts:5`.

`schemaFetch` returns `new CallPromise((resolve, reject) => doFetch(...)
.then(resolve, reject))`. A `new` expression is a value the return pass does
not follow, so the wrapper's answer is undecided, and the response of the
site at `Api.executeQuery` (`src/seed-promise-class-lib.ts:17`) is read at
the deciding call, whole.

| Row | Wanted response | Before (the head of carrick#4513) |
|---|---|---|
| `POST /svc/api/v1/query` at `src/seed-promise-class-lib.ts:17` | `{ format: "json"; results: unknown[]; } \| { format: "csv"; results: string; }` | `unknown` (`machinery_envelope`): read at the `fetch`, the `Response` is transport |

Main (before carrick#4513) reads it the same way, at the deciding call.

## Wrappers over the platform `fetch` (review of carrick#4805, finding 1)

Each wrapper is written in `src/<name>-lib.ts` and called at
`src/<name>.ts:2`. The summaries state each caller's row, and its wrapper
holds no transport, so the row is never asked for a response type: no
response entry stands at `src/<name>.ts:2`, on main and on this head. The
model's rows at the `fetch` lines keep main's stored text. P3 (the landing
and Q2) owns typing these callers.

| Row | Wrapper hands back | Response at `src/<name>.ts:2` |
|---|---|---|
| `fetch-two` | the second request's parse, after a first request | none (main: none) |
| `fetch-rev` | the first request's parse, after a second request | none (main: none) |
| `fetch-ping` | `res.ok` | none (main: none) |
| `fetch-flag` | `true` | none (main: none) |
| `fetch-env` | `{ status: 200, body }` | none (main: none) |
