# `library-store-deno`

The `library-store` fixture as Deno would install it (carrick#1720).
`tests/library_store_test.rs` copies `library-store`, removes its
`package-lock.json` and puts this `deno.lock` in its place, so the packages,
the sources, the cassette and the answer key are that fixture's.

The lockfile is in format 5, as Deno 2.9.6 writes it. It lists the same four
packages at the versions `node_modules` holds, each with its integrity, and
one JSR package:

- `@fixture/queue`, `@fixture/live` and `@fixture/beacon` came from the
  default registry, so they carry no `tarball`, and are sent.
- `fixture-private-bus` came from another host, which Deno records as its
  `tarball`. It is never sent.

With `NPM_CONFIG_REGISTRY` naming another host, nothing is sent, and the scan
states the rows `library-store` states with no claims.
