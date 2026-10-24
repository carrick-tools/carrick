# Discovery across services fixture (carrick#2618)

`repo/` has two services of one run and one package that belongs to none:

| Path | Service | Role |
|---|---|---|
| `services/app` | `app` | Its `tsconfig.json` loads `es2022` only (no DOM): it never calls `fetch` itself |
| `packages/client` | `client` | Sends through a local helper that calls `fetch`; its `tsconfig.json` loads `dom` |
| `packages/loose` | none | `ping()` calls `fetch`, but no service reads the file |

`library-claims.json` is the store's answer for the default library's
`fetch` (`lib.dom`).

## Answer key

Discovery of `app` with `client` as its sibling
(`discover_with`, `src/request_summary/discovery.rs`), for `services/app/src/main.ts`:

| Line | Call | Record |
|---|---|---|
| 5 | `getItem(id)` | A site. End: `fetch` in `packages/client/src/http.ts`. Chain: `getItem`, `send` |
| 9 | `postTo(…)` | A site. Chain: `postTo`, `send` |
| 14 | `items.remove(id)` | A site. Chain: `ItemsClient.remove`, `send`. No record at `new ItemsClient()` |
| 18 | `format(n)` | No record |
| 22 | `ping()` | `unfollowed`, `outside_service`: its file is in no service |

No record, site or unfollowed, names a file under `packages/`: a run records
only the calls of its own service's files. Without siblings, lines 5, 9 and
14 are `unfollowed: outside_service`.

Decided sites of the same walk (`decided_sites_with`,
`src/request_summary/decided_sites.rs`): lines 5, 9 and 14 are sites at the
app's own call, named by the client's code (`/v1/api/items/${id}`,
`/v1/api/events`, `/v1/api/items/${id}`). The client's calls are never the
site.

## The scan

`repo/__llm__/library-claims/default.json` is the store's cassette for the
same `fetch`, so `tests/sibling_sites_test.rs` scans the repo as the Action
does (`carrick .`, compiler rows and site questions on). The app's blob then
holds `GET /v1/api/items/:id` (line 5), `POST /v1/api/events` (line 9) and
`DELETE /v1/api/items/:id` (line 14) of `services/app/src/main.ts`, and no
row names a file of the other service. The app asks for `fetch` only because
the client does: the scan adds the services a walk may enter to the app's ask.
A `--changed-only` scan of a change to the app alone carries the client, walks
it for the app's calls, and states the same rows.
