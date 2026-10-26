# Discovery: a function written as a parameter's default (carrick#2813 slice S2)

`repo/services/web` has one service. Each file calls `fetch` (the `lib.dom`
global) through a function written as a parameter's default.

| File | Role |
|---|---|
| `src/arrow.ts` | `propose(base, doFetch = (url, init) => fetch(url, init))`, called with no `doFetch` |
| `src/control.ts` | The control: `doFetch = fetch`, the default is the global itself |
| `src/several.ts` | `load(base, doFetch = (url, init) => fetch(url, init))`: one caller passes nothing, another passes its own function |
| `src/destructured.ts` | `notify({ base, send = (url, init) => fetch(url, init) })` |
| `src/ctor.ts` | `constructor(private base, private send = (u, i) => fetch(u, i))`, a method calls `this.send(…)` |
| `src/plain.ts` | Negative: the default calls a local helper that sends nothing |

`library-claims.json` is the store's answer for the default library's `fetch`.

## Answer key

| File:line | Call | Record |
|---|---|---|
| `arrow.ts:7` | `doFetch(…)` | A site. End: `fetch` at `arrow.ts:5`. Chain: one `<value>` hop at line 5 |
| `arrow.ts:12` | `propose(…)` | A site, same end. Chain: `propose` (line 3), then the `<value>` hop |
| `control.ts:4` | `doFetch(…)` | A site; the end is the call itself, no hop |
| `several.ts:4` | `doFetch(…)` | A site, end `fetch` at line 3, stop `several_values` |
| `destructured.ts:7` | `send(…)` | A site, end `fetch` at line 5, a `<value>` hop at line 5 |
| `ctor.ts:10` | `this.send(…)` | A site, end `fetch` at line 6, a `<value>` hop at line 6 |
| `plain.ts` | `format(name)` | No end |

The call inside a default (`fetch(url, init)`) is no site of its own: the end
sits at the call that invokes the value.
