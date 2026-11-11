# `socket-decorated-handlers`

Fixture for carrick#3043: a message handler written as a method decorator.
`tests/socket_decorated_handlers_test.rs` runs it.

Everything in it is invented. `@fixture/gateway` 3.0.0 (public registry,
`package-lock.json`; declarations hand-written in the vendored `node_modules`)
exports six decorators and a client class, and the store's answer is the
cassette `__llm__/library-claims/default.json`:

- `OnMessage(name): MethodDecorator` (role socket, side server): one `receive`
  op, `member: null`, `on: export`, `form: decorate`, the event name in
  argument 0. The decorated method is the handler.
- `Link` (role socket, side client, made with `new`): `send` names the event in
  argument 0.
- `OnEvent<T = string>(message: T)` and `OnNothing<T>(message: T)` (same role, side and op as `OnMessage`): the first states `lobby.leave` at `gateway.ts:20`, the second states nothing (no default to read).
- `Traced(label): MethodDecorator`, `Gateway(): ClassDecorator` and
  `Peer(): ParameterDecorator` are not labelled.

Two services, `lobby` and `player`. Answer key:

| Site | Row |
|---|---|
| `lobby/src/gateway.ts:6` | endpoint `CLIENT->SERVER` `lobby.enter` (the method's name; decorator on line 5) |
| `lobby/src/gateway.ts:12` | endpoint `CLIENT->SERVER` `lobby.chat` (decorator on line 11, under an unlabelled one) |
| `lobby/src/gateway.ts:20` | endpoint `CLIENT->SERVER` `lobby.leave` (`OnEvent`, defaulted name) |
| `lobby/src/gateway.ts:23` | nothing: `OnNothing` has no default |
| `lobby/src/gateway.ts:16` | nothing: `@Traced("lobby.idle")` is not labelled |
| `player/src/client.ts:6` | call `CLIENT->SERVER` `lobby.enter` |
| `player/src/client.ts:10` | call `CLIENT->SERVER` `lobby.chat` |
| `lobby/src/wrapped.ts:20` | no row; one unread site (`own_function`, slot `name`, callee `Handles`): the method is decorated through an in-repo factory whose body applies `OnMessage` (carrick#3688 C1) |
| `lobby/src/wrapped.ts:23` | nothing: `Tagged` wraps the unlabelled `Traced`, so its body holds no labelled site |
