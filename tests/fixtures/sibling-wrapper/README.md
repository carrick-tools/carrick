# A client wrapper over a sibling service's functions (carrick#2618)

`repo/` has two services of one run, `services/app` (`app`) and
`packages/client` (`client`). The client's functions each call the global
`fetch` with a literal path. `library-claims.json` is the store's answer for
the default library's `fetch` (`lib.dom`).

The app reaches them three ways:

| Where | Call | Answer |
|---|---|---|
| `api.ts:6` | `ItemsApi.get` calls `getItem(id)`; `commands.ts:6` calls `ItemsApi.get` | A site, `/api/items/${id}`. Its row is withdrawn only once call-through states the request at `commands.ts:6` (the leaf row, ruled on carrick#2448), so the request keeps exactly one fact row |
| `api.ts:10` | `ItemsApi.status` calls `getStatus()`; `commands.ts:11` calls it | A site, `/api/status`, on the same terms |
| `api.ts:14`, `api.ts:15` | `ItemsApi.both` calls `getVersion()` and `getItem("1")` | A site each: the wrapper reaches two requests, so no caller states one |
| `commands.ts:21` | `health()` calls `getHealth()` itself | A site, `/api/health` |

No site and no lead is in a file under `packages/`: a run states a request
only at a call of its own service's files.

Call-through reads the requests from each service's mount graph, and the
rows the site questions state sit in the blob's `calls` only, so on this
fixture it states no leaf row and both wrappers keep theirs.
