# unread-through-listed-client

The answer key for `tests/unread_through_listed_client_test.rs` and the
discovery test
`a_call_through_a_listed_client_into_an_unlisted_package_is_the_listed_package_unmatched`:
a call declared in a package the scan never asked the library store about,
made on an instance a listed, networked package built, is saved as an unread
site with that listed package's reason, `package_unmatched`.

- `repo/` is one root service. It lists `@acme/sdk` (answered: `createClient`
  is an `http_client`, no ops), `@acme/store` (`createStore` answered `none`)
  and `@acme/extras` (answered with no exports). `@acme/functions-js` and
  `@acme/cache-js` are installed, never listed, never asked. Stubs sit under
  `node_modules/@acme/` (committed with `git add -f`); the lockfile names every
  stub.
- `expected.json`:
  - `src/invoke.ts:6` calls `client.functions.invoke(...)`, declared in the
    unlisted `@acme/functions-js`: `package_unmatched`.
  - `src/caller.ts:4` calls `welcome()`, which holds that call:
    `missed_inside` (the bound on how far the new reason travels).
  - `src/invoke.ts:10` calls `client.ping()`, declared in the listed
    package: `package_unmatched`, as before.
  - `src/invoke.ts:3` is `createClient`, answered with no op: `no_op_claim`
    (counted in `by_reason`, as before).
  - `src/store.ts:6` (a `none` answer) and `src/extras.ts:4` (declared in a
    listed package answered with no exports, so it was asked): no unread site.
  - `src/direct.ts:4` imports `@acme/direct-js` itself, which the service does
    not list: the import makes it an asked package the scan ends without an
    answer for, so the call is `unanswered_package`, as before.
  - `src/gone.ts:4` calls a package that is not installed: `value_callee`, as
    before.
