# expression-path-segment

Answer key for `tests/expression_path_segment_test.rs` (carrick#4514).
The cases are invented. The scaffold is `call-through-fact-helper`: two
services, `services/app` (`app`) and `packages/api` (`api`), and a labelled
client `@fixture/http`.

A segment the code writes as an expression is one placeholder. A choice
between literals is `:param`, never the name of one of its values. An env
read after a `/` is one segment of the path, never the base URL.

| Where | Code | Row | Why |
|---|---|---|---|
| `app` `kinds.ts:4` | `listThings(archived ? "archived" : "active")` | `GET /things/kinds/:param` | A choice holds no name. |
| `app` `kinds.ts:10` | `listThings(SITE)`, `SITE` an env choice | `GET /things/kinds/:param` | As above; not a `GET /` row. |
| `app` `kinds.ts:14` | `listThings("archived")` | `GET /things/kinds/archived` | One literal is stated as is. |
| `app` `kinds.ts:18` | `getThing(draft ? "draft" : "published")` | `GET /things/:param` | A choice. |
| `app` `more.ts:7` | `listThings(user.kind)` | `GET /things/kinds/:kind` | A member names its key. |
| `app` `more.ts:11` | `listThings(String(n))` | `GET /things/kinds/:n` | Unchanged. |
| `app` `more.ts:15` | `listThings(process.env.KIND as string)` | `GET /things/kinds/:KIND` | An env read in a path is a segment. |
| `app` `more.ts:19` | `listThings(k ?? "all")` | `GET /things/kinds/:k` | `??` is not a choice. |
| `api` `direct.ts:5` | `` `/direct/${archived ? "archived" : "active"}` `` | `GET /direct/:param` | A choice. |
| `api` `direct.ts:11` | `` `/direct/${MODE}` ``, `MODE` an env choice | `GET /direct/:param` | As above. |
| `api` `direct.ts:15` | `` `/direct/${process.env.API_VERSION}/users` `` | `GET /direct/:API_VERSION/users` | The prefix stays. |
| `api` `direct.ts:19` | `` `/direct/${kind ?? "all"}` `` | `GET /direct/:kind` | Unchanged. |
| `api` `direct.ts:31` | `"/concat/" + (archived ? ... : ...) + "/x"` | `GET /concat/:param/x` | A choice. |

`things.ts` rows are as in `call-through-fact-helper/README.md`.
