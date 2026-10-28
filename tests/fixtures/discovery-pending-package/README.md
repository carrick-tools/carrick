# Discovery: a walk that passed a package the store has not answered (carrick#2813 S1)

`repo/services/web` has one service. `library-claims.json` is the store's
answer: `fetch` (`lib.dom`) answered with a client export, `fixture-answered`
answered with no export, `fixture-pending` pending (still building).

| File | Role |
|---|---|
| `src/client.ts` | `send` and `sendViaClient` call `transmit` from `fixture-pending` |
| `src/events.ts` | `postEvent` calls `send`: a wrapper chain that ends in the pending package |
| `src/inject.ts` | `notify` calls `doSend`, which holds `deps.send ?? sendViaClient`: a value callee whose fallback ends in the pending package |
| `src/answered.ts` | Control: `show` calls `render`, which calls `format` from the answered package |
| `src/global.ts` | Control: `load` calls `request`, which calls the global `fetch` (answered): a site |

## Answer key

| File:line | Call | Record |
|---|---|---|
| `events.ts:4` | `send("/events", body)` | No end. `missed_inside`, and `unanswered_at` holds `fixture-pending` |
| `inject.ts:7` | `doSend("/notify", body)` | No end. `missed_inside` (not `value_callee`), and `unanswered_at` holds `fixture-pending` |
| `client.ts:4` | `transmit(path, body)` | A direct call into the package: Rule 4's, no `unanswered_at` |
| `answered.ts:8` | `render(body)` | `finished`, no `unanswered_at` |
| `global.ts:8` | `request("/items")` | A site, end `fetch`, no `unanswered_at` |
