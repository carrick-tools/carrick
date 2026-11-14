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
| 14 | `sendBoth(ports[name])` | none: a keyed value is a miss beside an end (not read) |
| 18 | `sendStopOnly(ports[name])` (twin of 14) | `missed_inside` |
| 22 | `sendBoth({ send: cb })` | `value_never_bound`, `stop_at` `mixed.ts:5` |
| 26 | `sendStopOnly({ send: cb })` (twin of 22) | `value_never_bound`, `stop_at` `mixed.ts:10` |
| 32 | `sendBoth(this.port)` | none (as 14) |
| 35 | `sendStopOnly(this.port)` (twin of 32) | `missed_inside` |
| 40 | `retryBoth(3)`, recursion beside the end | none |
| 44 | `sendAndRun(run)`, `run` is a parameter | `value_never_bound`, `stop_at` `mixed.ts:19` |
| 48 | `runOnly(run)` (twin of 44) | `value_never_bound`, `stop_at` `mixed.ts:24` |
| 52 | `sendAndRun(async () => undefined)`, a callback written at the call | none |
| 56 | `sendAndHook()`, a `let` never assigned in the sibling | `value_never_bound`, `stop_at` `mixed.ts:30` |
| 60 | `hookOnly()` (twin of 56) | `value_never_bound`, `stop_at` `mixed.ts:35` |
| 66 | `sendBoth(unsetPort)` | none (a miss, as 14) |
| 70 | `sendStopOnly(unsetPort)` (twin of 66) | `missed_inside` |

Lines 4, 22, 44 and 56 are the shapes carrick#4474 fixes. Lines 14, 32 and 66
are a miss beside an end, which reads nothing and is a separate question.
`packages/client`'s own blob is the control: its stops are saved at its own
calls, `mixed.ts:7`, `:11`, `:21`, `:25`, `:32` and `:36`.
