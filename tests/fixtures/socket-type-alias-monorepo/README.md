# socket-type-alias-monorepo

Both sides of one Socket.IO contract declare their socket through a type alias
of the imported socket type, declared in the same file (carrick#670):

- `services/runner` aliases the client socket and emits `run:start` and
  `run:stop` from fields declared with the alias.
- `services/gateway` aliases the per-connection server socket and registers
  the two handlers on a constructor parameter property declared with it.

`services/runner/src/notifier.ts` goes one step further: it never names the
socket type at all, importing the alias its sibling declares and listening for
`run:notify` on a field declared with it.

Before the alias was resolved, none of the three files produced a row, because
the declared-type rule admitted only the imported names literally. Every `__llm__`
cassette is empty: the rows exist only if the deterministic socket pass emits
them.
