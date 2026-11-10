# socket-inherited-listener

Answer key for `tests/socket_inherited_listener_test.rs` (carrick#3390).

`@fixture/live-wire` declares `Socket extends Emitter`, with `on` and `once`
inherited from `@fixture/emitter-base`. The store labels `on`/`once` only under
`@fixture/live-wire` `Socket` (`library-claims.json`), so a receiver that holds
a `Socket` must be read under that class, not under the base package that
declares the member.

The client service listens three ways; the server service emits each event
through `@fixture/live-server`.

| Event | Client listener | Held as |
| --- | --- | --- |
| `presence` | `client.ts:4` | module local built by `io()` (decided by the instance read already; control) |
| `ticker` | `client.ts:10` | constructor parameter property typed `Socket` |
| `notice` | `client.ts:22` | getter returning a `Socket` field |

Each is a `library_claim` endpoint row, direction `server_to_client`.

Protocols: socket listeners. The type is found through the file's import of
the receiver's type; a type reached with no import in the file (inferred from a
call's return, or `import type`) is not read, a gap.

## Typed receivers

`services/client/src/typed.ts` and `hooks.tsx` hold listeners whose receiver
type is not the class's printed name. `Socket` is generic here, and
`Socket#io` is a `Manager`, a second class that inherits `on`; the store gives
`Manager` no ops, and a function export named `io` has the instance ops.

| Event | Listener | Held as |
| --- | --- | --- |
| `w:alias` | `typed.ts:17` | getter returning an in-repo alias of the generic class |
| `w:tagged` | `typed.ts:22` | parameter typed by an in-repo alias of `Socket & {...}` |
| `w:type-import` | `typed.ts:25` | parameter typed by a type-only import renamed on import |
| `w:lifecycle` | `typed.ts:28` | `socket.io.on`: a property named like the `io` export. No row |
| `h:joined` | `hooks.tsx:12` | `.tsx` hook returning a closure over an alias intersection |

## A class that inherits the member but is not the labelled export

`@fixture/live-store` labels `on` under `default` and `Client`. Its `Client`
extends `Emitter`; its `Cursor`, written `export default class Cursor` in an
inner module and re-exported as `Cursor`, extends `Flow` from
`@fixture/flow-base`, which the store does not label.

| Event | Listener | Held as | Wanted |
| --- | --- | --- | --- |
| `s:message` | `store.ts:4` | parameter typed `Client` | `library_claim` row |
| `data` | `store.ts:9` | local from `client.cursor()`, a `Cursor` | no row, no lead |
| `s:bus` | `store.ts:15` | parameter typed by a repo class extending `Client` | `library_claim` row; a gap today: the class's `receiver_type` row names the repo class, not the labelled base, and the line reads as an in-process bus row (not asserted) |

`shared` (`store.ts:18`) constructs a `Client`, so the scan asks the store
about `@fixture/live-store`; a package imported only for its types is not
asked.

The compiler engine writes a `receiver_type` row at each receiver beside the
member row (`src/sidecar/test/position-rows-receiver-type.test.ts`). Until the
scanner reads those rows, `w:tagged` and `h:joined` read as in-process event
bus rows, `w:alias` is missed and `w:lifecycle` reads as a library listener.
