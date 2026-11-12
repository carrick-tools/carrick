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
