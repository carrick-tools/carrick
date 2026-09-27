# producer-wider

Synthetic two-service fixture for carrick#1516: a producer whose inferred
response type is wider than what its handler returns, and an untyped consumer
that declares what the handler really sends. `http-client` and `http-server`
are hand-written declarations, not real packages.

- `api/src/routes.ts` answers `GET /holidays` with the rows
  `api/src/holidays.ts` maps. The mapping returns `'specific'` or `'all'` for
  `scope`, which TypeScript infers as `scope: string`.
- `web/src/holidays.ts` calls `GET /holidays` with no type argument (line 13)
  and hands the response to a setter declaring `scope: 'all' | 'specific'`.

Used by `producer_type_wider_than_its_handler_returns_is_its_own_class` in
`src/engine/type_compat_v2.rs` (inference, capture, definitions, check and
retype directly).
