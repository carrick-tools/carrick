# Value resolver: forms the S0 lift already follows (carrick#3504)

`repo/services/web` has one service. `src/request_summary/resolve.rs`'s
tests read the callee of each `send(…)` call through the resolver with a
follower that names each end (an in-repo definition, a function written in
place, or nothing it can read). The forms are the ones discovery's value
walk followed before the lift; the other forms these tests read sit in the
`discovery-default-function` and `discovery-factory-function` fixtures.

## Answer key

| File | Call | Values the resolver gives |
|---|---|---|
| `src/cycle.ts` | `send(…)` in `relay` | `sendItems` once: `relay` passes `send` to itself unchanged, which adds nothing |
| `src/spread.ts` | `send(…)` in `go` | `sendGo` (from `direct`) and nothing (from `forward`, whose spread moves every position) |
| `src/spread.ts` | `send(…)` in `lonely` | `sendGo`: nothing calls `lonely`, so it holds its default |
| `src/short_circuit.ts` | `(on && sendOne)(…)` in `maybe` | `sendOne`: `a && b` is `b` or a falsy `a`, and only `b` is read |
