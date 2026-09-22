# file-route-cassette

Astro endpoints whose routes are stated by the file layout, with a handler
that reads a body and returns a typed value (carrick#1400).

The file raises no candidate at a route's registration, because an exported
function is not a call. Every candidate it offers is a call a handler makes: a
body read, a form read, an outbound `fetch`. The model still has to echo an id,
so this is where the join has to tell the route's own row from a borrowed site.

`__llm__/` is recorded from one real analyzer run and is never hand-written.
`__golden__.json` is the replayed projection, and
`tests/file_route_cassette_gate_test.rs` asserts it. To re-record both after
an intentional change:

```bash
cargo build --release
scripts/record-cassette.sh tests/fixtures/file-route-cassette
```
