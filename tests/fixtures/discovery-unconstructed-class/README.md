# Discovery: a class its own service never constructs (carrick#2808)

`repo/services/client` is a client package: its classes are built by another
service (`repo/services/app/src/use.ts`), so no file of `client` holds a `new`
of them. Each holds a request function in a private field, with the language's
or a package's function as the fallback.

| File | Role |
|---|---|
| `src/items.ts` | `ItemsClient`: `#send = options.send ?? globalThis.fetch.bind(globalThis)`; `list()` calls `#request`, which calls `#send` |
| `src/events.ts` | `EventsClient`: `#publish = options.publish ?? publish` (`fixture-bus`, a broker); `emit()` calls `#publish` |
| `src/bare.ts` | Control: `#send = options.send`, no fallback |
| `src/local.ts` | Control that must not change: `LocalClient` shaped like `ItemsClient`, constructed in this service with a stub that sends nothing |
| `src/index.ts` | A barrel re-exporting the classes |

`library-claims.json` is the store's answer for `fetch` (`lib.dom`) and
`fixture-bus` (`publish`, op `send`, name at argument 0, payload at argument 1).

## Answer key

| File:line | Call | Record |
|---|---|---|
| `items.ts:13` | `this.#request("/items")` | A site. End: `fetch` |
| `items.ts:17` | `this.#send(…)` | A site. End: `fetch` (the constructor's default) |
| `events.ts:13` | `this.#publish(…)` | A site. End: `publish` in `fixture-bus`, role broker |
| `bare.ts:11` | `this.#send(…)` | No end: the parameter has no default |
| `local.ts:11` | `this.#send(…)` | A site, end `fetch`, stop `several_values` (the stub and the default): read through its construction, as before |
