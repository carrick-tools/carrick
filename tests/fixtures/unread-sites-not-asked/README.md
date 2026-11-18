# unread-sites-not-asked

The answer key for `tests/unread_sites_not_asked_test.rs` and the discovery
test `a_call_declared_in_a_package_never_asked_is_not_asked_when_the_scan_ends`:
a call declared in an installed package the service does not list is saved as
an unread site with the reason `package_not_asked`, where it was dropped with
no reason before.

- `repo/` is one root service that lists `@acme/sdk` only. The SDK's client
  exposes `functions`, declared in `@acme/functions-js`, which the service does
  not list and the store is never asked about. Both stubs sit under
  `node_modules/@acme/` (committed with `git add -f`). `library-claims.json`
  is the store's answer: `@acme/sdk` answered with no export.
- `expected.json`:
  - `src/invoke.ts:6` calls `client.functions.invoke(...)`: `package_not_asked`.
  - `src/invoke.ts:10` calls `client.ping()`, declared in the listed
    package: no unread site (answered with nothing to label).
  - `src/gone.ts:4` calls a package that is not installed at all: its reading
    is the one it had before, `value_callee`.
