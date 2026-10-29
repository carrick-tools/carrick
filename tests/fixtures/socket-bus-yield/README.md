# `socket-bus-yield`

Fixture for carrick#1670 T4: in-process event-bus rows and the model's
pub/sub twins give way to every socket row the scan states, the site rows
included. `tests/socket_bus_yield_test.rs` runs it.

The package is invented, and its declarations are hand-written in the
vendored `node_modules`: `@fixture/live-wire` 3.1.0 (public registry,
`package-lock.json`), a `Live` class made with `new` whose instances `emit`
and listen with `on`.

The cassette:

- `__llm__/framework-detect/framework-detect.json` lists no socket client,
  so the socket pass reads nothing here and the event pass reads both calls.
- `__llm__/library-claims/default.json` is the store's answer: `Live` is a
  socket export, side client, with `emit` (send) and `on` (receive) on its
  instances, the name in argument 0.
- `__llm__/analyze-file/twin.json` is the model reading the listener in
  `src/twin.ts` as a pub/sub subscriber on `chat.read`, at the handler's
  line (5), not the call's (4).

Both calls are on a parameter (`announce(s: Live)`, `listen(s: Live)`), so
no library row is read there; with the five site switches on, each is a
decided site whose row is a socket row.

Answer key, with the switches on:

| Site | Row |
|---|---|
| `src/announce.ts:4` | `socket|CLIENT->SERVER|chat.sent` call, `library_claim`; the event pass's publisher there gives way |
| `src/twin.ts:4` | `socket|SERVER->CLIENT|chat.read` endpoint, `library_claim` |
| `src/twin.ts:5` | nothing: the model's `pubsub|chat.read` row is the socket row's twin in the same file |

With the switches off there are no site rows, and the scan states the event
pass's `pubsub|chat.sent` publisher at `src/announce.ts:4` and the model's
`pubsub|chat.read` subscriber at `src/twin.ts:5`.
