# spread-absent-key (fixture for ruling R-SP, carrick#2448 / #3504)

One root service. A REST client written into the repository in the shape an
OpenAPI generator writes (`client/`, `instance.ts`, `sdk.ts`), plus the query
hooks such a generator writes beside it (`hooks/queries.ts`). Every SDK
function sends through `(options?.client ?? client).get({ url, ...options })`.
The store labels the language library's `fetch` and, for `deps_spread/`, the
exports of a table store client package (`node_modules/fixture-table`): the
client, and the `GetRow` and `QueryRows` commands it sends.

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
| `through_value/meters.ts:15` | `log.warn(...)`, `log` read from `meterWith(deps, { ...base, scope })` at `:23` and `:24`, `base = { log, tag }` with `log` the pool meter's parameter; `meterPools` is run only as a value | `unread_expression`, as before the rule: each spread write keeps its unread reading when the parameter's callers hold nothing |
| `through_value/meters.ts:23`, `:24`, `:33` | the calls into the meter | no site of their own: the walk stops inside the meter, as before the rule |
| `through_value/refusals.ts:9` | `log.reject(...)`, `log` read from `refusal({ ...args, note })` in the gate and in an arrow inside it; the gate is run only as a value | `unread_expression`, as before the rule |
| `through_value/panel.ts:19` | `this.onEvent(...)`, the field set from the constructor's options, built only as `new Panel({ token, ...params })` in a method this service never calls | `value_callee`, as before the rule, never `value_never_bound` |
| `through_value/relay.ts:9` | `log.warn(...)`, `log` read through `leaf({ ...opts })` in `mid`, called as `mid({ ...args })` by a relay run only as a value | the reason `gates.ts:35` states: two spreads on the way up are one unread write, as before the rule |
| `through_value/builder.ts:8` | `onInput(...)`, `handle` read through a builder method's `makeTask({ ...options, tag })`, `options` a parameter of the method written inside the builder | `injected_callee`: the spread is read, and the method's parameter is filled by what runs the method (the rule's read; main says `value_callee`) |
| `deps_spread/action.ts:9`, `:10` | `deps.loadRow(key)`, `deps.listRows()`; `runAction` is called with `tableDeps()` and with `{ ...tableDeps(), mint }`, the factory's members each sending a table command (`:10` inside a callback a paging helper runs) | a site reaching the command's label (`GetRow`, `QueryRows`) with no stop: read through the spread, both callers hold the same members (before the rule: `several_values`, the end kept) |
| `deps_spread/action.ts:15`, `:20` | `deps.loadRow(key)` and `load(key)`, the one caller handing `JSON.parse(raw)` | saved with the call's own reason, `value_callee`, on both sides of the rule: the one held stop is never dropped silently |
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
`a_key_through_a_parameter_run_only_as_a_value_keeps_the_unread_reason`,
`a_key_a_spread_source_writes_from_an_unfilled_parameter_keeps_mains_reason`,
`a_member_read_through_a_spread_of_a_factory_keeps_its_labelled_end`,
`a_callee_holding_one_stop_is_saved_with_a_reason`.
