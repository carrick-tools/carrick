# unread-sites-unready

The answer key for `tests/unread_sites_unready_test.rs` (carrick#3684): a call
into a library package the scan ended without an answer for is saved as an
unread site with the reason `unanswered_package`.

- `repo/` is one root service with three stubbed packages under
  `node_modules/@fixture/` (committed with `git add -f`). The store answer
  replayed for it is `library-claims.json`: `@fixture/http` answered,
  `@fixture/pending` pending (`dispatch_capped`, a reason no wait ends, so the
  offline scan does not wait), `@fixture/failed` skipped with `build_failed`.
- `expected.json`:
  - `src/pending.ts` and `src/failed.ts` call into the pending and the failed
    package: `unanswered_package`.
  - `src/outer.ts` calls `announce` from `pending.ts`: `missed_inside`, and
    `also` holds `unanswered_package`.
  - `src/failed.ts` also states `cause`, the store's `failed_reason` for the
    build-failed package (carrick#3739); `src/pending.ts` states none.
  - `src/stated.ts` states its path: no unread site.
  - `by_reason` counts and `boundary` holds the pending and failed counts,
    with no `library_ask_failed`.

`tests/fixtures/unread-sites/` is a different fixture: the cloud pins its
`blob.expected.json` by hash.
