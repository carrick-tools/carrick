# `socket-per-connection`

Fixture for the fold of a fact socket row against the socket pass's row at the
same line. `tests/socket_per_connection_test.rs` runs it.

Everything in it is invented. `socket.io` and `socket.io-client` 4.8.1 are
named in `package-lock.json`; their declarations are hand-written in the
vendored `node_modules`. The store's answer is the cassette
`__llm__/library-claims/default.json`: `socket.io` `Socket` is the server's end
and `Namespace` a client end, and `socket.io-client` `Socket` the client's end.
`__llm__/framework-detect` lists no socket client, so only the socket pass
(which reads the `socket.io` imports) and the fact layer read these files.

| Site | Socket pass | Fact layer | Row |
|---|---|---|---|
| `srv/src/server.ts:6` | `CLIENT->SERVER chat.in` | the same | endpoint, `library_claim` |
| `srv/src/server.ts:9` | `SERVER->CLIENT chat.hello` | the same | call, `library_claim` |
| `srv/src/server.ts:13` | `SERVER->CLIENT chat.reply` | the same | call, `library_claim` |
| `srv/src/server.ts:18` | none (a `Map` element) | `SERVER->CLIENT chat.fan` | call, `library_claim` |
| `srv/src/server.ts:23` | `SERVER->CLIENT chat.notice` | `CLIENT->SERVER chat.notice` | call, `socket_call`: the old row stays |
| `cli/src/client.ts:4` | `CLIENT->SERVER chat.in` | the same | call, `library_claim` |
| `cli/src/client.ts:5-8` | `SERVER->CLIENT` listeners | the same | endpoints, `library_claim` |
