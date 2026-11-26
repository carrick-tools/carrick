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
- `Traced(label): MethodDecorator`, `combine(...decorators): MethodDecorator`,
  `Gateway(): ClassDecorator` and `Peer(): ParameterDecorator` are not
  labelled.

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
| `lobby/src/wrapped.ts:20` | endpoint `CLIENT->SERVER` `w:wrapped`, nothing saved: the method is decorated through an in-repo factory whose returned value applies `OnMessage` (carrick#3781) |
| `lobby/src/wrapped.ts:23` | nothing: `Tagged` wraps the unlabelled `Traced`, so its body holds no labelled site |
| `lobby/src/wrapped.ts:13` | nothing saved: every method decorated through `Handles` is read |
| `lobby/src/composed.ts:27` | endpoint `CLIENT->SERVER` `w:combined` (the factory hands back the unlabelled `combine` over `OnMessage`) |
| `lobby/src/composed.ts:30` | no row; one unread site (`own_function`, slot `name`, callee `Logs`): `Logs` calls `OnMessage` but hands back `Traced` |
| `lobby/src/composed.ts:33` | endpoint `CLIENT->SERVER` `w:const` (the decorator passes a module constant) |
| `lobby/src/composed.ts:4`, `:21` | nothing saved: `Listens` and `Named` are read at every method |
| `lobby/src/imported.ts:5` | endpoint `CLIENT->SERVER` `w:imported` (the factory is in `decorators.ts`, imported) |
| `lobby/src/decorators.ts:12` | nothing saved |
| `player/src/client.ts:14`, `:18`, `:22`, `:26` | calls `CLIENT->SERVER` `w:wrapped`, `w:combined`, `w:const`, `w:imported` |
