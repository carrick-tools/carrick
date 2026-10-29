# unwidened-budget

Synthetic two-service monorepo for carrick#2100: a producer that returns a
literal the compiler widens, and a consumer that expects the literal.
`http-client` and `http-server` are hand-written declarations, not real
packages.

- `api/src/jobs.ts` answers `GET /job` with `{ status: "ready", attempts: 1 }`,
  unannotated, so the published response is `{ status: string; ... }`. The
  unwidened reading recovers `status: "ready"`.
- `web/src/job-client.ts` calls it as `api.get<Job>("/job")`, where `Job`
  states `status: "ready"`.
- `__llm__/` holds the mocked model answers for a `CARRICK_MOCK_ALL` scan.

Used by `tests/unwidened_budget_test.rs`, which scans it with the sidecar's
cap on readings forced to 0 (`CARRICK_UNWIDENED_READINGS_CAP`) and with the
default one.
