# Discovery: a factory that returns a function it declares (carrick#2432 slice A2)

`repo/services/web` has one service:

| File | Role |
|---|---|
| `src/http.ts` | `makeClient` declares `request`, returns it as `Send`; `request` calls `doFetch`, which holds `options.fetch ?? globalThis.fetch.bind(globalThis)` |
| `src/items.ts` | Two calls through the returned function |
| `src/plain.ts` | The control: a factory returning a nested function that calls no network |

`library-claims.json` is the store's answer for the default library's `fetch`
(`lib.dom`).

## Answer key

| File:line | Call | Record |
|---|---|---|
| `items.ts:5` | `send("POST", …)` | A site. End: `fetch` in `http.ts`. Chain: one `<value>` hop at `request` (`http.ts:9`). Stop `several_values`: `options.fetch` is a key no caller fills, so the other value decides |
| `items.ts:10` | `send("GET", …)` | A site. Same end and chain |
| `plain.ts:10` | `label("hello")` | Not a site (`unfollowed`, `value_callee`) |
