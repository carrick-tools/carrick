# new-url-through-wrapper

A request wrapper, `sendRequest(input, init)` in `src/net/transport.ts`, hands
its options to `fetch` through a helper with two returns, `init ?? {}` and
`{ ...init, dispatcher: agent }`. Its callers build the URL with
`new URL('/path', origin)`, where the origin is opaque. The request
summaries must read the method each caller writes through that spread, and
state each caller's request keyed on the path alone (carrick#1561 C11: the
`new_url_target` rule that used to patch these rows is deleted). The test is
`tests/new_url_through_wrapper_test.rs`.

The cassettes hold what a model reports here: the origin glued to the front
of the path (`${API_ORIGIN}/api/releases`), a host the source never states.

Answer key (every row `request_summary`, host-free):

- `src/releases.ts:5` `POST /api/releases/confirm` (the URL object, origin a
  destructured parameter).
- `src/releases.ts:14` `POST /api/releases` (`url.href`).
- `src/releases.ts:20` `POST /api/bundles/preview` (`url.toString()`).
- `src/releases.ts:26` `POST /api/bundles/:name/upload` (`` `${url}` ``).
- `src/usage.ts:9` `POST /api/usage` (the `new URL` written in the wrapper's
  argument).
- `src/usage.ts:16` `GET /api/ping` (control: the `new URL` handed to
  `fetch` itself).
- `src/usage.ts:21` `POST /api/heartbeat` (`sendTimed`: a one-return
  helper, `{ ...init, signal }`).
- `src/usage.ts:26`: no `request_summary` row. `sendUpdate`'s helper may
  write `method: 'PUT'` over the caller's `POST`, so the summaries state no
  method there (the model's row stands).

Types at the caller: `releases.ts:5` sends `{ version: string; notes:
string; }` and casts `{ confirmed: boolean; }`; `releases.ts:14` sends
`{ version: string; }` and casts `{ id: string; }`; `usage.ts:9` sends
`{ command: string; }`.
