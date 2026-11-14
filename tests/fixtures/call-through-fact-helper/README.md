# call-through-fact-helper

Answer key for `tests/call_through_fact_helper_test.rs` (carrick#4372).
The cases are invented.

`repo/` has two services of one run, `services/app` (`app`) and
`packages/api` (`api`). `api` sends each GET through its own generic
helper, `request<T>(path)`, which calls the labelled client
`@fixture/http` (`__llm__/library-claims/default.json`). So each call of
the helper holds a same-file wrapper row, and the fact layer states the
same request there.

| Where | Call | What stands | Why |
|---|---|---|---|
| `api` `things.ts:13` | `request<Thing>(\`/things/${id}\`)` | `GET /things/:id`, request summary; graph role `network_request`, client `request` | The fact replaces the same-file wrapper row of its own operation. Its graph row keeps what the wrapper row read off the call: this call is the request. |
| `api` `things.ts:17` | `request<Thing[]>(\`/things/kinds/${kind}\`)` | `GET /things/kinds/:kind`, as above | As above. |
| `api` `things.ts:21` | `http.post(...)` | `POST /things/:id`, request summary | The labelled call itself. |
| `app` `things.ts:4` | `getThing(draft ? "drafts/latest" : "published/latest")` | `GET /things/:id`, request summary; graph role `wrapper_call`, reaches `packages/api/src/things.ts:13` | The argument holds two paths, so the fact layer states nothing here and saves the site as unread. The call-through walk states the row from `getThing`'s request row in `api`. |
| `app` `things.ts:8` | `listThings(archived ? "old/archived" : "new/active")` | `GET /things/kinds/:kind`, reaches `packages/api/src/things.ts:17` | As above. |
| `app` `things.ts:12` | `saveThing(thing)` | `POST /things/:id`, request summary | The fact layer reads it through `saveThing`. |

Before the fix, the fact's graph row at `api` `things.ts:13` and `:17`
read as a call through (`wrapper_call`, reaching no request), so the walk
from the app found no request there and the app's two GET rows were lost.
