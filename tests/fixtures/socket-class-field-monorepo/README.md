# socket-class-field-monorepo

Two services whose Socket.IO contract is only visible through class fields
(carrick#659):

- `services/supervisor` builds the client socket in one method, parks it on
  `private notifications?: Socket<…>`, and emits `run:subscribe` /
  `run:unsubscribe` from two other methods.
- `services/gateway` hands each accepted connection to a class that keeps it
  on `private readonly socket: Socket` and registers the listeners there.

Neither side writes an op on a local binding, so before #659 the pass found
no root and the whole contract was invisible. Every `__llm__` cassette is
empty on purpose: the rows exist only if the deterministic socket pass emits
them.
