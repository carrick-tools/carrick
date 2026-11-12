# spread-absent-key (fixture for ruling R-SP, carrick#2448 / #3504)

One root service. A REST client written into the repository in the shape an
OpenAPI generator writes (`client/`, `instance.ts`, `sdk.ts`), plus the query
hooks such a generator writes beside it (`hooks/queries.ts`). Every SDK
function sends through `(options?.client ?? client).get({ url, ...options })`.
The only store label is the language library's `fetch`.

The rule under test: a key read through object spreads is absent when every
spread source resolves (through callers, under the existing caps) to an
object that does not write that key. If any source does not resolve, the key
stays unread. `x ?? y` with `x` absent reads `y` alone.

| File:line | Call | Wanted |
|---|---|---|
| `sdk.ts:7`, `:14`, `:22` | `(options?.client ?? client).get/post(...)` | a site reaching `fetch`, no stop: no caller's object writes `client` |
| `hooks/queries.ts:8`, `:15`, `:22` | `listGadgets({ ...options, signal, throwOnError: true })`, `options` the hook's own parameter | a site reaching `fetch`, no stop: the spread source is the outer function's parameter, and no caller writes `client` |
| `callbacks.ts:17`, `:21` | `listWidgets({ ...key[0], signal })`, `listWidgets({ ...fnOptions })` | `several_values`: the sources are parameters of callbacks a library runs, not resolvable in this repository, so `client` stays unread (filed under the V1 shape hold) |
| `unread/stored.ts:8`, `:15` | `listStored({ ...JSON.parse(raw), signal })` | `several_values`: the spread source is not read, so `client` stays unread |
| `writes/offline.ts:16`, `:23` | `listOffline({ ...base, signal })`, `base = { client: offline }` | `several_values`: the spread source writes `client`, a client that sends nothing |
| `through_value/gates.ts:9`, `:23` | `log.warn(...)`, `log` read from `check({ ...args, tag })` and from `{ ...base, tag }` with `base = { ...args }`; `runGate` is run only as `deps.gate(...)` (`dispatch.ts`, which writes `log`) | the reason `gates.ts:35` states, never `value_never_bound`: no direct caller of `runGate` is found, so the key keeps the unread reading it had before the rule (carrick#4025) |
| `through_value/gates.ts:35` | `log.warn(...)`, `log` read through `...JSON.parse(raw)` | the walk's reason for an unread key, on both sides of the rule (the twin) |
| `writes/same.ts:7` | `options.run(...)`, the only caller `call({ ...base, signal })`, `const base = { run: send }` local to the caller | a site with no stop: the spread source writes `run`, so the key reads `send` and the call enters it |

A module-level `base` spread whole stays unread, as the module-binding census
(R5) reads a binding only where every module reads a member of it; that is
not changed here.

Tests: `src/request_summary/discovery.rs`,
`a_key_no_spread_source_writes_is_absent`,
`a_key_an_unread_spread_source_may_write_keeps_several_values`,
`a_key_a_library_callback_parameter_spread_keeps_several_values`,
`a_key_a_spread_source_writes_is_read`,
`a_key_a_spread_source_writes_another_client_keeps_several_values`,
`a_key_through_a_parameter_run_only_as_a_value_keeps_the_unread_reason`.
