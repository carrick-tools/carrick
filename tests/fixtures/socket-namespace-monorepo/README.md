# socket-namespace-monorepo

The publishers and the handler of one Socket.IO event, where the handler is
registered on a custom namespace (carrick#662):

- `services/platform` builds its server in one function and carves the
  namespace off it in another, so the `Namespace` type annotation is the only
  thing that says what the binding is. Its `connection` handler registers
  `run:subscribe` and `run:unsubscribe`.
- `services/worker` holds a client socket on a class field and emits both
  events, naming the namespace only in the URL path it connects with.

Neither side can put the namespace in the operation key, so both are recorded
under the plain event name and meet there. Every `__llm__` cassette is empty:
the rows exist only if the deterministic socket pass emits them.
