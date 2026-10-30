# `socket-origin`

Fixture for carrick#1670 T2: an export the store marks `by_origin` has no side
of its own, so a call on one takes the side its instance came from.
`tests/socket_origin_test.rs` runs it.

Everything in it is invented. `@fixture/rt` 2.0.0 (public registry,
`package-lock.json`; declarations hand-written in the vendored `node_modules`)
has three socket classes, and the store's answer is the cassette
`__llm__/library-claims/default.json`:

- `Hub` (side server, made with `new`): `on` runs a function and hands it a
  `Peer`. The op names no event, handler or `yields`, as Socket.IO's
  `Server.on` does not.
- `Peer` (side client, `side_rule: by_origin`, made with `new`): `send` and
  `listen`, the event name in argument 0. The members are not `emit` and `on`
  so that no in-process event-bus row stands at these lines.
- `Gate` (side client): `watch` runs a function and hands it a `Peer` as its
  second parameter, and its op says so in `yields`, with side server.

`__llm__/framework-detect/framework-detect.json` lists no socket client, so the
socket pass reads nothing here.

Two services, `hub` and `peer`. Answer key, with the five site switches on:

| Site | Where the `Peer` came from | Row |
|---|---|---|
| `hub/src/server.ts:6` | `hub.on("connection", (peer) => …)`, written in place | endpoint `CLIENT->SERVER` `order.placed` |
| `hub/src/server.ts:9` | the same callback | call `SERVER->CLIENT` `order.accepted` |
| `hub/src/server.ts:16` | `hub.on("connection", onPeer)`, handed by name | call `SERVER->CLIENT` `order.rejected` |
| `hub/src/server.ts:20` | `notify(peer)`, called from the callback | call `SERVER->CLIENT` `order.shipped` |
| `hub/src/server.ts:26` | `gate.watch((info, peer) => …)`, `yields` param 1 side server | call `SERVER->CLIENT` `order.gated` |
| `peer/src/client.ts:5` | `new Peer(…)` | call `CLIENT->SERVER` `order.placed` |
| `peer/src/client.ts:6` | `new Peer(…)` | endpoint `SERVER->CLIENT` `order.accepted` |
| `peer/src/client.ts:11` | a parameter nothing in the service hands in | call `UNKNOWN` `order.lost` |

carrick#1670 T3 adds `hub/src/conn.ts` and `hub/src/attach.ts`: a field set from
one constructor parameter is that parameter, so its class's `new` calls say
where the `Peer` came from. All five classes are built in `attach.ts` with the
peer a second `Hub` hands in, but `Orphan`, built with a parameter nothing
hands in.

| Site | The field | Row |
|---|---|---|
| `hub/src/conn.ts:10` | `Conn`, a parameter property | endpoint `CLIENT->SERVER` `run.start` |
| `hub/src/conn.ts:16` | `Conn` | call `SERVER->CLIENT` `run.notify` |
| `hub/src/conn.ts:29` | `Plain`, one `this.peer = peer` | call `SERVER->CLIENT` `run.done` |
| `hub/src/conn.ts:42` | `Two`, written again in a method | call `UNKNOWN` `run.ping` |
| `hub/src/conn.ts:53` | `Leaky`, `this` handed to a call | call `UNKNOWN` `run.flush` |
| `hub/src/conn.ts:63` | `Orphan`, built with a peer nothing hands in | call `UNKNOWN` `run.lost` |

The two services pair on `order.placed` and `order.accepted`. Every row is a
`library_claim`. With the switches off there are no site rows and the scan
states no socket row.
