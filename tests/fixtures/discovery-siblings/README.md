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

## A walk that writes no rows

When the client's compiler walk is asked and writes no rows (it failed, or died
of memory), the app's calls into the client's code cannot be read. The app's
unread sites at lines 5, 9, 14 and 18 of `main.ts` read `walk_lost`, with
`stop_at` in `packages/client/src/http.ts`, beside the one `outside_service`
site at line 22 (`unread_sites_by_reason` is `{"outside_service": 1,
"walk_lost": 4}`). The app then states only lines 5 (GET) and 14 (DELETE): the
POST at line 9 is absent while the client has no rows. The client's own five
unread sites read `walk_lost` when its walk fails in a full scan, and its
`boundary.walk_rows_lost` says why. A service whose walk was never asked (no
compiler engine) keeps `rows_missing`. `tests/sibling_sites_test.rs` holds both
scans and `discovery.rs` the same read without a scan.
