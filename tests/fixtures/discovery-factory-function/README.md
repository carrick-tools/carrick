# Discovery: a factory that returns a function it declares (carrick#2432 slice A2)

`repo/services/web` has one service:

| File | Role |
|---|---|
| `src/http.ts` | `makeClient` declares `request`, returns it as `Send`; `request` calls `doFetch`, which holds `options.fetch ?? globalThis.fetch.bind(globalThis)` |
| `src/items.ts` | Two calls through the returned function |
| `src/bound.ts` | `makeBound` declares `request` like `makeClient`, but holds `options.fetch.bind(options)`: the bound object is a factory parameter the closure captures; `request`'s own first parameter also has a `fetch` key |
| `src/bound_items.ts` | One call through the returned function, whose argument fills `fetch` with the global `fetch`; the factory was given a function that sends nothing |
| `src/plain.ts` | The control: a factory returning a nested function that calls no network |

`library-claims.json` is the store's answer for the default library's `fetch`
(`lib.dom`).

## Answer key

| File:line | Call | Record |
|---|---|---|
| `items.ts:5` | `send("POST", …)` | A site. End: `fetch` in `http.ts`. Chain: one `<value>` hop at `request` (`http.ts:9`). Stop `several_values`: `options.fetch` is a key no caller fills, so the other value decides |
| `items.ts:10` | `send("GET", …)` | A site. Same end and chain |
| `plain.ts:10` | `label("hello")` | Not a site (`unfollowed`, `value_callee`) |
| `bound_items.ts:9` | `send({ … })` | Not a site with an end. `options` is the factory's parameter, one function out of `request`; reading it as `request`'s own first parameter takes the argument's global `fetch` |
