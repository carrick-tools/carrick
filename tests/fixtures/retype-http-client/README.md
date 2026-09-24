# retype-http-client

Synthetic two-service monorepo for carrick#1491: a consumer that calls a
generic HTTP client instance with and without a type argument, and reads a
field off the response. `http-client` and `http-server` are hand-written
declarations, not real packages. The client's `post<T>` resolves to an envelope
that carries `T` as `data` beside a request-data parameter defaulting to `any`,
which is the shape that leaves both calls without a comparable type.

- `api/src/routes.ts` answers `POST /checkout` with a `CheckoutResult { y }`.
- `web/src/checkout.ts` reads `response.data.x` after a typed call (line 6) and
  after an untyped one (line 11), and discards the response of a third call
  (line 16), which the retype cannot judge and the scan logs with its reason.
- `__llm__/` holds the mocked model answers for a `CARRICK_MOCK_ALL` scan.

Used by `tests/retype_consumer_test.rs` (the whole scanner) and by
`untyped_consumer_call_is_judged_by_retyping_it` in
`src/engine/type_compat_v2.rs` (capture, check and retype directly).
