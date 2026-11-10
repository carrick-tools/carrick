# unread-sites

The answer key for `tests/unread_sites_test.rs` (carrick#3688 U3): every call
site the fact layer considered and did not state as a row is on the index blob
as `unread_sites`, with its reason.

- `repo/` is one root service with an HTTP client stub
  (`node_modules/@fixture/http`, committed with `git add -f`). The store answer
  replayed for it is `library-claims.json`.
- `expected.json`: each file of `reasons` holds an unread site with that
  reason; each file of `none` holds none.
  - `value-callee.ts`: the callee is an in-repo value that is no definition.
  - `declared.ts`: the callee is a `declare function`, a declaration with no
    body and no implementation answer (`interface_member`).
  - `recursion.ts`: the walk reaches a function it already passed.
  - `possible-call.ts`: the path is a parameter of a function nothing calls;
    a possible call as well.
  - `stated.ts`: the path is written; a fact row, no unread site.
- `blob.expected.json`: the scanned blob's `possible_calls`, `unread_sites`,
  `unread_sites_total` and `unread_sites_by_reason`. The cloud's readers pin
  this file by sha256, so a change here moves their pin too.

- `boundary.expected.json` (U4): a boundary with unread sites and the lines
  `ServiceBoundary::lines` prints for it. The Rust test
  `an_unread_site_count_is_a_boundary_line` writes it (set
  `CARRICK_WRITE_FIXTURES=1`); `npm/carrick/test/render.test.ts` reads it and
  the port must print the same lines.

Not here yet: route-side stops (U6), the model-answer reasons (unit tests in
`src/site_rows/tests.rs`), and the reasons no source file can cause (unit
tests in `src/unread_sites.rs`).
