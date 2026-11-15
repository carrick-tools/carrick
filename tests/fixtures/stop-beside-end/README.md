# Stop beside an end fixture (carrick#4474)

`repo/` has the shape of `discovery-siblings`: `services/app` (no `fetch` of
its own), `packages/client` (a service whose functions send) and one loose
package. `packages/client/src/mixed.ts` holds functions that send a request
and also run a value nothing binds; each has a twin that only runs the value.

`library-claims.json` is the store's answer for the default library's
`fetch` (`lib.dom`).

## Answer key

The app's calls in `services/app/src/relay.ts`. Every call with an end has a
row; the unread-site column is what the app saves at the same call.

| Line | Call enters | Unread site at the call |
|---|---|---|
| 4 | `sendBoth(port)`, `port.send` is a parameter | `value_never_bound`, `stop_at` `mixed.ts:5` |
| 8 | `sendStopOnly(port)` (twin of 4) | `value_never_bound`, `stop_at` `mixed.ts:10` |
| 14 | `sendBoth(ports[name])` | `unread_expression`, `stop_at` `mixed.ts:5` (the value handed in is a form the resolver does not read) |
| 18 | `sendStopOnly(ports[name])` (twin of 14) | `unread_expression`, `stop_at` `mixed.ts:10` |
| 22 | `sendBoth({ send: cb })` | `value_never_bound`, `stop_at` `mixed.ts:5` |
| 26 | `sendStopOnly({ send: cb })` (twin of 22) | `value_never_bound`, `stop_at` `mixed.ts:10` |
| 32 | `sendBoth(this.port)` | as 14 |
| 35 | `sendStopOnly(this.port)` (twin of 32) | as 18 |
| 40 | `retryBoth(3)`, recursion beside the end | none |
| 44 | `sendAndRun(run)`, `run` is a parameter | `value_never_bound`, `stop_at` `mixed.ts:19` |
| 48 | `runOnly(run)` (twin of 44) | `value_never_bound`, `stop_at` `mixed.ts:24` |
| 52 | `sendAndRun(async () => undefined)`, a callback written at the call | none |
| 56 | `sendAndHook()`, a `let` never assigned in the sibling | `value_never_bound`, `stop_at` `mixed.ts:30` |
| 60 | `hookOnly()` (twin of 56) | `value_never_bound`, `stop_at` `mixed.ts:35` |
| 66 | `sendBoth(unsetPort)` | as 14 |
| 70 | `sendStopOnly(unsetPort)` (twin of 66) | as 18 |

Lines 4, 22, 44 and 56 are the shapes carrick#4474 fixes. Lines 14, 32 and 66
hand the sibling a value the resolver does not read (carrick#4486): the call
saves the resolver's reason beside its row, and the twins say the same.
`packages/client`'s own blob is the control: its stops are saved at its own
calls, `mixed.ts:7`, `:11`, `:21`, `:25`, `:32` and `:36`.

## Inside, own files and pass-throughs (carrick#4486)

`services/app/src/inside-relay.ts`: the receiver is written in the sibling
(`packages/client/src/inside.ts`), so it was already saved, and these lines
are unchanged. `:11`, `:12` and `:13` call stop-only helpers of the app's
own (`local.ts`), the control for the sibling-file gate: a hole handed into
an own file keeps its words.

| Line | Call | Unread site at the call |
|---|---|---|
| 4 | `keyedBoth(name)` | `interface_member`, `stop_at` `inside.ts:5` |
| 5 | `keyedOnly(name)` | `interface_member`, `stop_at` `inside.ts:10` |
| 6 | `r.fieldBoth()` | `value_never_bound` |
| 7 | `r.fieldOnly()` | `value_never_bound` |
| 11 | `oneOnly(ports[name])` (one caller) | `missed_inside` |
| 12 | `twoOnly(ports[name])` (two callers) | `missed_inside` |
| 13 | `twoOnly(ports[name])` | `missed_inside` |

In `local.ts`, `:4` (`oneOnly`'s `port.send`, one caller) reads `value_callee`
and `:8` (`twoOnly`'s, two callers) reads `unread_expression`.

`services/app/src/pass.ts`: own pass-throughs that call into the sibling and
are handed `ports[n]`. `:3` and `:4` have two callers and `:5` and `:6` one.
Each reads `unread_expression` with `stop_at` `mixed.ts:5` (`:3`, `:5`) or
`mixed.ts:10` (`:4`, `:6`).

## A specific reason beside a hole (carrick#4486, narrowed)

`services/app/src/specific-relay.ts` hands `ports[name]` (a hole) as the
first argument and its own never-bound `run` parameter as the second, into
`packages/client/src/specific.ts`. The hole's reason fills a site that saved
no reason; it never replaces one the walk saved.

| Line | Call | Unread site at the call |
|---|---|---|
| 6 | `sendThenRun(ports[name], run)` (reaches an end) | `unread_expression`, `stop_at` `specific.ts:3` |
| 10 | `thenRunOnly(ports[name], run)` (twin) | `value_never_bound`, `stop_at` `specific.ts:9` (not `unread_expression`) |
