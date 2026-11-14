# sibling-chained-sends

carrick#4445. `app` (`services/app`) relays one `kit` function per line and
never calls `.add`, `.get` or `.postMessage` itself, so the receiver trace's
declaration ask lists none of those members. `kit` (`packages/kit`) sends
through a value another call made.

Answer key, `services/app/src/relay.ts` (the app's blob), floor only (the
`missed_inside` floor, without the wider declaration ask):

| Line | Kit call | Answer |
|---|---|---|
| 2 | `new Queue('kit-literal').add` | unread `missed_inside`, slot `callee` |
| 3 | held `kit-held` (control) | `unanswered`, slot `kind` |
| 4 | held `kit-kept` plus chained `kit-chained` | `unanswered`, slot `kind`; the chained send is lost and not shown (one row per call span) |
| 5, 6 | construction only; `close()` (controls) | nothing |
| 7 | `createClient(...).get`, chained | the old path's row `GET /kit/chained`, plus unread `missed_inside` until the reading lands |
| 8 | `createClient(...).get`, held | the old path's row `GET /kit/held`; a held local is not linked to its maker, so no floor row |
| 9 | `new BroadcastChannel('kit-bus').postMessage` | unread `missed_inside`, slot `callee` |

`controls-relay.ts` lines 2 and 3 (a `Queue` construction beside
`target.addEventListener`; a `BroadcastChannel` construction beside
`window.addEventListener`) hold nothing: the floor links a passed call's
receiver to the maker, it does not match by member name.

Traps: keep each send on its own line (a site is `file:line`); an npm stub
needs a `package-lock.json` entry or the store never asks for it.

With the declaration ask widened to the sibling's members (T1, carrick#4445),
line 2 is a possible call (`pubsub producer`, `unanswered`) and an unread site
`unanswered`, slot `kind`, with the labelled end `Queue.add`. Lines 3 to 8 do
not move. Line 9 stays with the floor: the app loads no DOM lib, so the bus
send is not verified from the app's root.
