# value-run-param (fixture for carrick#4025)

One root service. A function is handed to another function as an object
property and run only through that value (`deps.g(args)`), so no call names
it. The value it hands on reaches a parameter read by `log.warn(...)` in a
function it calls. The store labels nothing.

| File:line | Call | Wanted |
|---|---|---|
| `local.ts:8` | `log.warn('x')`, `log` a parameter of `inner`, called by `gate`, which is handed as `{ g: gate }` and run only as `deps.g(args)` in the same module | a saved site whose reason is never `value_never_bound`: the call that runs `gate` writes `log`. The site is kept among the sites, stopped as `callback_into_own_function`: `gate` is handed to a function of this code, and the call that runs it is not read |
| `gates.ts:7` | `log.warn('checked')`, `log` a parameter of `check`, called by `runGate`, which `wiring.ts` imports, hands as `{ gate: runGate }` and runs only as `deps.gate(args)` | the same as `local.ts:8` |
