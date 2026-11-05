# A client wrapper over a sibling service's functions (carrick#2618)

`repo/` has two services of one run, `services/app` (`app`) and
`packages/client` (`client`). The client's functions each call the global
`fetch` with a literal path. `library-claims.json` is the store's answer for
the default library's `fetch` (`lib.dom`).

The app reaches them three ways:

| Where | Call | Answer |
|---|---|---|
| `api.ts:6` | `ItemsApi.get` calls `getItem(id)`; `commands.ts:6` calls `ItemsApi.get` | A site, `/api/items/${id}`. Call-through states the request at `commands.ts:6` (the leaf row, ruled on carrick#2448), and the wrapper's row is withdrawn, so the request keeps exactly one fact row |
| `api.ts:10` | `ItemsApi.status` calls `getStatus()`; `commands.ts:11` calls it | A site, `/api/status`, on the same terms: the row is at `commands.ts:11` |
| `api.ts:14`, `api.ts:15` | `ItemsApi.both` calls `getVersion()` aside and returns `getItem("1")` | A site each. Its caller, `commands.ts:16`, states the request `both` returns, the one `getItem` writes, with the value `both` fills in (`/api/items/1`), and `api.ts:15` is withdrawn; the aside request keeps its row at `api.ts:14` |
| `commands.ts:21` | `health()` calls `getHealth()` itself | A site, `/api/health` |

No site and no lead is in a file under `packages/`: a run states a request
only at a call of its own service's files.

Call-through reads the requests from each service's mount graph, where the
fact site rows replace the reader's rows at the same line (carrick#2795).

`tests/sibling_sites_test.rs` scans `repo/` as a cloud scan does (every
fact-layer switch on), with the store's answer in
`repo/__llm__/library-claims/`, once as it is (the reader states nothing in
`api.ts`) and once with `reader/analyze-file/api.json` copied in (the reader
states model rows at `api.ts:6` and `api.ts:10`). Both times the app's blob
holds exactly the five rows above, each `request_summary`.

With the fact-layer switches off, the mock reader's answer for the client's
`http.ts` names each `fetch` one line low (it counts the line after the
source fence as line 1), so model rows no fact replaces stand beside the
requests, every wrapper reaches a request only the model states, and
call-through states no leaf row: the five rows sit at the wrappers' calls
instead.
