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

A wrapper that writes its URL local more than once (`src/clients/branches.ts`,
callers in `src/use-branches.ts`, test `tests/url_local_branches_test.rs`)
states one request per write at each caller. A write under a test of the
wrapper's own parameter (`===` / `!==` against text, `startsWith`, an anchored
literal pattern) is kept or dropped by the text the caller, or a hop between,
fills it with; under any other test (`if (data)`) every write is kept.

Answer key (the row at each call line in `use-branches.ts`):

- 18, set in both branches of a prefix test, called with a path: `POST
  /api/items.list` only.
- 19, initialised then set again under a prefix test: `POST /api/items.list`.
- 20, a query set under a truthiness test, or the path: every row keys `POST
  /api/items.list`.
- 21, line 18's client called with an absolute URL: only that URL is sent,
  an outside host the scan states no call row for (as an inline `fetch` of
  one), so no row; never a based `/api/https://…`.
- 22, set again inside a callback: no row.
- 23, the branching client (a method test filled one hop away, then an
  anchored pattern on the path): `POST /api/items.list` only.
- 24, the URL set from a query the same statement sets: no row.
