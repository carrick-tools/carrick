# `listener-receivers`

Fixture for carrick#941 and carrick#2133: which `<receiver>.on("event", …)`
listeners are pub/sub endpoints. Driven by `tests/listener_receivers_test.rs`.

The `__llm__/analyze-file` answers report nothing, so every pub/sub row in the
blob is the in-process event-bus pass's (`src/event_emitter.rs`). Framework
detection (`__llm__/framework-detect`) lists `@fixture/broker` as the one
messaging client. No package is installed: the scan only needs
`package.json` to say it is a dependency.

| Site | Receiver | Row |
|---|---|---|
| `orders.ts:7` `orderPlaced` | `bus`, imported from `./bus`, which builds it with `new EventEmitter()` | kept: an in-repo import |
| `invoices.ts:5` `invoicePaid` | `new Subscriber(process.env.BROKER_URL)` from `@fixture/broker` | kept: a transport detection lists |
| `stdin-reader.ts:6` `line` | `readline.createInterface({ input: process.stdin })` | none: a runtime module's call result |
| `worker-output.ts:7` `line` | `createInterface({ input: child.stdout })`, chained | none: a runtime module's call result |
