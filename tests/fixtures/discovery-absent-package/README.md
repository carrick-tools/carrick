# Discovery: a package the store's answer holds no entry for (carrick#3992 T2a)

`repo/services/web` has one service. `library-claims.json` is the store's
answer: `fixture-answered` answered with no export. It holds no entry at all
for `fixture-graph`, which the service imports and installs.

| File | Role |
|---|---|
| `src/mailbox.ts` | `profile` calls through a `#private` field and `inbox` through a public field, both holding a `fixture-graph` client |
| `src/answered.ts` | Control: `show` calls `render`, which calls `format` from the answered package |

## Answer key

| File:line | Call | Record |
|---|---|---|
| `mailbox.ts:13` | `this.#graph.api("/me").get()` | With `fixture-graph` ended without: `unfollowed` with `unanswered_package` |
| `mailbox.ts:17` | `this.open.api("/me/messages").get()` | The same |
| `answered.ts:8` | `render(body)` | `finished`: answered with nothing to label stays silent |
