# A client wrapper over a sibling service's functions (carrick#2618)

`repo/` has two services of one run, `services/app` (`app`) and
`packages/client` (`client`). The client's functions each call the global
`fetch` with a literal path. `library-claims.json` is the store's answer for
the default library's `fetch` (`lib.dom`).

The app reaches them three ways:

| Where | Call | Answer |
|---|---|---|
| `api.ts:6` | `ItemsApi.get` calls `getItem(id)`; `commands.ts` calls `ItemsApi.get` | No site. The wrapper has one call into the sibling and a caller in another file, which states the request at its own call |
| `api.ts:10` | `ItemsApi.status` calls `getStatus()`; `commands.ts` calls it | No site, for the same reason |
| `api.ts:14`, `api.ts:15` | `ItemsApi.both` calls `getVersion()` and `getItem("1")` | A site each: the wrapper reaches two requests, so no caller states one |
| `commands.ts:21` | `health()` calls `getHealth()` itself | A site, `/api/health` |

No site and no lead is in a file under `packages/`: a run states a request
only at a call of its own service's files.
