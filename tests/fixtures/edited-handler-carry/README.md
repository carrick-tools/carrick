# edited-handler-carry: an edited file keeps the model's answer for its unchanged lines

A producer registers its routes on a typed receiver (`api.get('/x', handler)`),
which no deterministic reader states, so the three routes exist only as the
model's answer. The cassettes under `cassettes/analyze-file/` are that answer.
`tests/local_mode_test.rs` is the only consumer (carrick#2894).

- `items-api/src/routes.ts` registers three routes: an inline return-value
  handler (line 4), an inline imperative-send handler (line 9) and a named
  handler (line 14, body at line 18).
- `items-api/src/server.ts` mounts the plugin under `/api/v1`.
- `items-api/src/openapi.ts` documents the same three operations by `$ref`, so
  the handler rows fold into `descriptor_route` facts and a mismatch fails.
- `items-web/` reads each route through a client member and states the
  response type it expects.

## Answer key

| edit | what a no-model scan must do |
|---|---|
| two lines above the plugin, `connected` renamed in the line-4 handler | carry all three rows (now at lines 6, 11, 16), re-type from the tree, and fail `check --changed` on the line-6 route |
| the line-9 path literal changed | not carry that row (`registration_line_changed`), carry the other two |
| the named handler's body changed | not carry the line-14 row (`hint_outside_registration_changed`), carry the other two |

Cassette line numbers are the fixture's line numbers: changing a line in
`routes.ts` changes this key in the same commit. The receiver is named `api`,
not `app`, because the mock reader answers `app.get("/` lines on its own.
