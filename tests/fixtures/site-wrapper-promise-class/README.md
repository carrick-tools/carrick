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
