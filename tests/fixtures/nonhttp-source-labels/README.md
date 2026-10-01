# `nonhttp-source-labels`

Fixture for carrick#1626: which source a pub/sub or socket row states, where
its producer evidence comes from, and which line it sits on. Driven by
`tests/nonhttp_source_labels_test.rs`.

Framework detection (`__llm__/framework-detect`) lists `@fixture/broker` as
the one messaging client. The `__llm__/analyze-file` answers report one
pub/sub operation, the publish in `orders.ts`; every other row here is read
by the scanner's own passes. No package is installed.

## Answer key

| Row | Side | Site | `resolution_source` | `provenance` | Why |
|---|---|---|---|---|---|
| `pubsub\|orders.created` | calls | `src/orders.ts:6` | `model` | default | the model stated it |
| `pubsub\|orders.cancelled` | endpoints | `src/orders.ts:10` | absent | `route` | the scanner backfilled it into the model's list; it is not the model's |
| `pubsub\|orders.shipped` | endpoints | `src/orders.ts:15` | absent | `route` | as above; on the `.subscribe(` line, not the `await client` line |
| `pubsub\|payments.settled` | endpoints | `src/mocks/broker.ts:5` | absent | `mock` | a producer under a mock tree |
| `socket\|CLIENT->SERVER\|chat:send` | endpoints | `src/realtime.ts:7` | absent | `route` | on the `.on(` line; the event pass leaves the span to it |

No `pubsub|chat:send` row: the socket listener claims its span.

A second scan of the unchanged tree takes the incremental path and states the
same rows: the backfill marker is never written to the cached answers, and the
backfill runs again on them.
