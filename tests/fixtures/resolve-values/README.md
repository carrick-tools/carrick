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
| `src/spread.ts` | `send(…)` in `go` | `sendGo` (from `direct`) and `unread_expression` (from `forward`, whose spread moves every position) |
| `src/reassigned.ts` | `s(…)` in `run` | nothing calls `run`, so `s` holds its default, the module binding `send`: `sendA` (the initialiser) and `reassigned`, since the destructuring write `[send] = …` is one no reader places (carrick#3688 U2) |
| `src/unread_twice.ts` | `send(…)` in `go` | `unread_expression` twice: both callers spread their arguments. Discovery states that one stop for the call, not `several_values` (carrick#3688 U2) |
| `src/spread.ts` | `send(…)` in `lonely` | `sendGo`: nothing calls `lonely`, so it holds its default |
| `src/short_circuit.ts` | `(on && sendOne)(…)` in `maybe` | `sendOne`: `a && b` is `b` or a falsy `a`, and only `b` is read |
| `src/scoped.ts` | `send(…)` in `inBlock` | `sendBlock`: a `const` in an `if` block holds its initialiser (carrick#3794) |
| `src/scoped.ts` | `pick()(…)` in `declaredInBody` | `sendPicked`: `pick` is declared in the body, and its return is read |
| `src/scoped.ts` | `local(…)` in `calledByName` | the function `local`, declared in the body and called by its name |
| `src/scoped.ts` | `held(…)` in `insideCallback` | `callback_parameter`: the local holds the callback's own parameter, which no caller index keys |
