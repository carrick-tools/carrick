# hosted-tables

A web app that reads and writes a hosted platform's tables through its
client: `db.from("<table>")` names the table, and the builder member called
on what `from` returns (`select`, `insert`, `upsert`, `update`, `delete`)
says what the request does. `supabase/config.toml` marks the repository as
the platform's. The client package's name is the platform's real one, with a
minimal declaration of our own, because the hosted conventions select the
served package by name. The builder package, `@fixture/hosted-rest`, and
every table, function and file name are invented.

`@fixture/hosted-rest` declares the builder and is not declared by the app:
it comes in through the client package, as in the real library.

## The store's answer (`library-claims.json`)

The tests copy it to `__llm__/library-claims/default.json`.

- `createClient` makes a client on argument 0 (its base).
- A `from` scope on the client, its name in argument 0.
- On what `from` returns (`instance:()>scope:from`), one `request` op per
  builder member, named by the scope (`name: {bound: "scope"}`), with the
  prefix `/rest/v1/` and the one method the member sends
  (`method_default`): `select` GET, `insert` POST, `upsert` POST,
  `update` PATCH, `delete` DELETE.
- A second `select` op, `method_default: "HEAD"`, that applies only where
  argument 1's `head` key holds `true` (`when`).
- `rpc` on the client: a `request` op named by argument 0, prefix
  `/rest/v1/rpc/`, `method_default: "POST"`.

## Answer key (consumer rows)

`VITE_HOSTED_URL` is the repository's own address (the client is made on it
and the repository serves the platform), so every path is host-free.

| File:line | Row | `label_member` |
|---|---|---|
| `src/read.ts:4` | `GET /rest/v1/accounts` | `select` |
| `src/write.ts:4` | `POST /rest/v1/notes` | `insert` |
| `src/written.ts:4` | `POST /rest/v1/notes`, one row: the `.select()` after `insert` is the filter builder's, not a second request | `insert` |
| `src/upsert.ts:4` | `POST /rest/v1/accounts` | `upsert` |
| `src/edit.ts:4` | `PATCH /rest/v1/notes` | `update` |
| `src/edit.ts:8` | `DELETE /rest/v1/notes` | `delete` |
| `src/count.ts:4` | `HEAD /rest/v1/accounts` (`{ head: true }`) | `select` |
| `src/name.ts:6` | `GET /rest/v1/notes` (the name is a module constant) | `select` |
| `src/stale.ts:4` | `GET /rest/v1/ghosts` (a table the type file lacks) | `select` |
| `src/fn.ts:4` | `POST /rest/v1/rpc/bump_count` | none (not on a scope's result) |

- `src/maybe.ts:5`: no row. Its `head` key is a parameter, so the call sends
  HEAD or GET at run time. Saved as an unread site, reason
  `several_operations`, on the labelled member `select`.
- `src/files.ts:4`: no row (storage is not claimed).
- `src/client.ts`, `src/types.ts`: no row.

## Producer rows

The type file `src/types.ts` states the tables (`accounts`, `notes`,
`unused_table`) and the function `bump_count`. Producer rows from it are a
separate ticket and are not asserted here.
