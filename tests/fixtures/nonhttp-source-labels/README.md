# `nonhttp-source-labels`

Fixture for carrick#1626: which source a pub/sub or socket row states, where
its producer evidence comes from, and which line it sits on. Driven by
`tests/nonhttp_source_labels_test.rs`.

Framework detection (`__llm__/framework-detect`) lists `@fixture/broker` as
the one messaging client. The `__llm__/analyze-file` answers report one
pub/sub operation, the publish in `orders.ts`. The library store's answer
(`__llm__/library-claims`) labels `connect().subscribe(topic)` as a receive,
which is what states the other three pub/sub rows. `node_modules` holds only
the package's `.d.ts`.

## Answer key

| Row | Side | Site | `resolution_source` | `provenance` | Why |
|---|---|---|---|---|---|
| `pubsub\|orders.created` | calls | `src/orders.ts:6` | `model` | default | the model stated it |
| `pubsub\|orders.cancelled` | endpoints | `src/orders.ts:10` | `library_claim` | `route` | the store labels `subscribe` as a receive |
| `pubsub\|orders.shipped` | endpoints | `src/orders.ts:15` | `library_claim` | `route` | as above; on the `.subscribe(` line, not the `await client` line |
| `pubsub\|payments.settled` | endpoints | `src/mocks/broker.ts:5` | `library_claim` | `mock` | a receive under a mock tree |
| `socket\|CLIENT->SERVER\|chat:send` | endpoints | `src/realtime.ts:7` | `socket_call` | `route` | on the `.on(` line; the event pass leaves the span to it. Newly stated (carrick#3091): the socket pass names itself, no fixture edit |

No `pubsub|chat:send` row: the socket listener claims its span.

A second scan of the unchanged tree takes the incremental path and states the
same rows.
