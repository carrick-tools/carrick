# wrapper-caller-stops

A caller of a function the service declares, whose request the send side could
not write as a row, is saved as an unread site with a reason (carrick#3688 F).
The test is `tests/wrapper_caller_stops_test.rs`.

Answer key:

- `src/use-stores.ts` line 6: the base class sends to a route that reads a
  field its subclass writes. No row; an unread site at the call, slot `path`,
  with the labelled `fetch` it reaches.
- `src/use-plain.ts` line 4: the wrapper states its route and the caller
  passes a literal. A row; no unread site.
