# `library-store`

Golden fixture for carrick#1664: the scan asks the library store about the
registry packages its library calls go through, and states a library row
wherever the type sidecar verifies the answer against the package's own
declarations. `tests/library_store_test.rs` runs it.

The packages are invented, and their declarations are hand-written in the
vendored `node_modules`. `package-lock.json` records where each came from:

- `@fixture/queue` 2.4.1 (public registry): a `bus` export with `publish`
  and `subscribe`.
- `@fixture/live` 1.2.0 (public registry): a `Socket` class whose
  instances `emit` and listen with `on`.
- `@fixture/beacon` 0.9.0 (public registry): a socket class the store
  answers `skipped`.
- `fixture-private-bus` 1.0.0, resolved from a private host: never sent.

The cassette:

- `__llm__/framework-detect/framework-detect.json` lists the socket and
  messaging clients, so the socket pass reads `@fixture/live` and
  `@fixture/beacon` sockets with unknown direction.
- `__llm__/library-claims/default.json` is the store's answer, in its wire
  shape: the queue's ops on the export, the socket's maker and its two ops on
  its instances (client side), and `@fixture/beacon` skipped.

Answer key, with the store's answer:

| Site | Row |
|---|---|
| `src/emails.ts:3` | `pubsub|orders.created` subscriber, a library row (the bus pass's row folds into it) |
| `src/orders.ts:4` | `pubsub|orders.created` publisher, a library row |
| `src/live.ts:5` | `socket|SERVER->CLIENT|chat` listener, a library row (the socket pass's `UNKNOWN` row folds into it) |
| `src/live.ts:10`, `:14` | `socket|CLIENT->SERVER|typing` and `join` emitters, library rows |
| `src/presence.ts:5` | `socket|UNKNOWN|presence`, the socket pass's row: no claims for its package |
| `src/audit.ts` | nothing: its package is never sent |

With no claims, or a refusal, the same scan states the socket pass's four
`UNKNOWN` rows and the bus pass's subscriber, and nothing else changes.
