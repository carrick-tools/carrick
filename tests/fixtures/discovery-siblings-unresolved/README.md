# Discovery across services through an unresolved import (carrick#3085)

`repo/` is a workspace (`package.json` `workspaces`) with three services of
one run:

| Path | Service | Role |
|---|---|---|
| `services/app` | `app` | Imports `@fixture/client`, but its `tsconfig.json` maps no path and no install holds the package: the compiler does not resolve the import, so the call graph follows it through the workspace's package name (carrick#2557). No row of the app names a client file |
| `packages/client` | `client` | Sends through a local helper that calls `fetch` (as in `discovery-siblings`) |
| `services/worker` | `worker` | Imports `@fixture/client` through a `paths` mapping the compiler resolves: its rows name the client |

`__llm__/library-claims/default.json` is the store's cassette for the
default library's `fetch`.

## Answer key

The app's blob holds, at its own calls, what the client's code sends:

| Line of `services/app/src/main.ts` | Call | Row |
|---|---|---|
| 4 | `getItem(id)` | `GET /v1/api/items/:id` |
| 8 | `postTo("/api/events", …)` | `POST /v1/api/events` |

The same rows whichever walk ends first: the app's rows may call through an
unresolved import, so its analysis waits for every analysed service's walk,
and through those walks' rows for the carried services they reach. A
`--changed-only` scan of a change to the app and the worker carries the
client, fills it with the rows the full scan kept once the worker's walk
names it, never walks it, and states the same rows.
