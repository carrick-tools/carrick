# wrapper-callers

A request wrapper that writes its URL into a local first (`const target =
base + path`, then `fetch(target)`) states its request at each caller the same
way the wrapper that writes the URL inline in `fetch(..)` does. Every local in
`src/clients/local-base.ts` has an inline twin; the callers are in
`src/use-local-base.ts`. The test is `tests/url_local_base_test.rs`.

Answer key (the row at each call line in `use-local-base.ts`):

- Local and inline twin of an unstated base beside a path parameter (a
  getter call, an environment read, an option or field): the same row.
- A local holding a literal piece: as it always read.
- A local of unstated pieces with no parameter: written by the local's name.

## Two-hop wrappers (`src/clients/api-module.ts`)

One module's request wrapper writes its URL into a local; object members call
it with a path; page components (`src/pages/entity-page.ts`) call the members.
Each page caller keeps its row, and reads as its inline twin does. The test is
`page_callers_of_a_url_local_wrapper_keep_their_rows` in
`tests/url_local_base_test.rs`.

## A URL local written more than once (`src/clients/branches.ts`)

A wrapper that writes its URL local more than once (`src/clients/branches.ts`,
callers in `src/use-branches.ts`, test `tests/url_local_branches_test.rs`)
states one request per write at each caller. A write under a test of the
wrapper's own parameter (`===` / `!==` against text, `startsWith`, an anchored
literal pattern) is kept or dropped by the text the caller, or a hop between,
fills it with; under any other test (`if (data)`) every write is kept.

Answer key (the row at each call line in `use-branches.ts`):

- 20, set in both branches of a prefix test, called with a path: `POST
  /api/items.list` only.
- 21, initialised then set again under a prefix test: `POST /api/items.list`.
- 22, a query set under a truthiness test, or the path: every row keys `POST
  /api/items.list`.
- 23, line 20's client called with an absolute URL: only that URL is sent,
  an outside host the scan states no call row for (as an inline `fetch` of
  one), so no row; never a based `/api/https://…`.
- 24, set again inside a callback: no row.
- 25, the branching client (a method test filled one hop away, then an
  anchored pattern on the path): `POST /api/items.list` only.
- 26, the URL set from a query the same branch sets just before: the read
  of the query sees that write, and both ways key `POST /api/items.list`,
  one row.
- 27 and 28, a test of an options key a hop writes before spreading its
  caller's options (`{ mode: "plain", ...options }`), the options omitted or
  written without the key: the hop's text decides the test, so `POST
  /api/items.list` only.
- 29, the same caller writing the key (`{ mode: "next" }`): `POST
  /api/v2/items.list` only.

## Exits and falsy fallbacks (`src/clients/exits.ts`)

The same reader around `return`, `throw`, an empty or `null` initialiser, a
loop, a closure, a read of the local inside its own write, and a test in a
nested block (callers in `src/use-exits.ts`, the same test file). Which
writes reach a read is the value resolver's flow
(`src/request_summary/resolve/flow.rs`).

Answer key (the row at each call line in `use-exits.ts`):

- 32, a branch writes the local and returns: `POST /api/items.list` only.
- 33, the same under `verb === "GET"`, called with `"GET"`: no row (the
  branch the caller takes returns).
- 34, `""` initialiser, then `withQuery || path`, called with `"POST"`:
  `POST /api/items.list`.
- 35, the same with a `null` initialiser: `POST /api/items.list`.
- 36, `""` initialiser, then `withQuery ?? path`: `POST /api` (`""` is not
  nullish).
- 37, written after the request: `POST /api/items.list`.
- 38, written in a loop: no row.
- 39, read by a closure before a later write: no row.
- 40, a branch writes the local and throws: `POST /api/items.list` only.
- 41, no initialiser, written only under `verb === "GET"`, called with
  `"POST"`: no row.
- 42, `target = target + "?q=1"` under `verb === "GET"`, through a hop
  passing `"POST"`: `POST /api/items.list`.
- 43, the same called with `"GET"`: one row keying `GET /api/items.list`.
- 44, the test and the read in one block below the declaration, called
  with `"POST"`: `POST /api/items.list`.
- 45, a query set under a truthiness test (`if (q) url = "/api/c4?query=1"`):
  the URL waits on no caller, so its row is stated at the send
  (`src/clients/exits.ts` line 170): one row keying `GET /api/c4`.

## Tests on the path to a request (`src/clients/path-guards.ts`)

A request after an `if` on a parameter whose branch exits (`return`,
`throw`), or inside the branch of one, whatever its URL is written as
(carrick#4093). A caller whose text takes the other way is not given the
row; a caller that passes something the scan does not read as text keeps
it. The tests on the path are the value resolver's flow
(`src/request_summary/resolve/flow.rs`, `tests_at`).

Answer key (the row at each call line in `src/use-path-guards.ts`):

- 13, `if (verb === "GET") return` before the send, called with `"GET"`:
  no row.
- 14, the same called with `"POST"`: `POST /api/items.list`.
- 15, the same called with the caller's own parameter, and nothing calls
  the caller: `POST /api/items.list`.
- 16, a helper called in the branch of `kind === "PUSH"` and after it,
  called with `"PUSH"`: `POST /api/sync/push` only.
- 17, the same called with `"PULL"`: `POST /api/sync/pull` only. The
  helper's own calls (`src/clients/path-guards.ts` lines 24 and 26) keep
  their rows.
- 18, `opts.mode.startsWith("dry")` throws before the send, called with
  `{ mode: "dry-run" }`: no row.
- 19, the same with `{ mode: "live" }`: `POST /api/items.list`.
- 20, a literal URL after `if (verb !== "PUT") return`, called with
  `"GET"`: no row.
- 21, the same called with `"PUT"`: `PUT /api/literal`.
