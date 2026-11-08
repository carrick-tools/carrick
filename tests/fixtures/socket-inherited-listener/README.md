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
