# sibling-own-install

carrick#4472. `sibling-chained-sends` (relay lines 1 to 9 unchanged) with the
installs split between the services:

- The root keeps `bullmq` 5.12.0. The kit has its own `bullmq` 5.13.0 under
  `packages/kit/node_modules`; its `index.d.ts` is two comment lines longer, so
  its declarations sit two lines lower. Both claims files hold a 5.13.0 twin of
  the `bullmq` entry (the mock store answers by version).
- `fixture-fetcher` is installed only under `packages/kit/node_modules`.
- `services/app/src/own-queue.ts:3` is the app's own `new Queue('app-own').add`,
  on the root's copy.
- The kit's tsconfig loads `dom`; the app's does not.

Answer key, `services/app/src/relay.ts` (the app's blob), mock scan:

| Line | Kit call | Main after carrick#4445 | With the sibling asked from its own root (carrick#4472 T1) |
|---|---|---|---|
| 2 | `new Queue('kit-literal').add` | unread `missed_inside`, slot `callee` | possible pubsub producer, `unanswered`; unread `unanswered`, slot `kind`, labelled end `Queue.add` |
| 7 | `createClient(...).get`, chained | row `GET /kit/chained` plus unread `missed_inside` | row `GET /kit/chained` only |
| 9 | `new BroadcastChannel('kit-bus').postMessage` | unread `missed_inside` | unread `missed_inside` (the DOM lib is the kit's; T3 reads it) |

`own-queue.ts:3` is read on every column: a possible pubsub producer and an
unread `unanswered`, slot `kind`. Asking the kit's pairs from the kit's root
alone would verify `bullmq` at the kit's copy only, so the app's own call is
asked from its own root first and the answers are merged by declaration.
