# base-path-gives-way

Answer key for `tests/base_path_gives_way_test.rs` (carrick#4434).
`@fixture/http` is labelled in `library-claims.json`, so the fact layer
states a row at every call. Where the call's env or literal origin is also
read from its spelling, a base-path pass states a row at the same call. The
cases are invented.

| File | Call line | What stands | Why |
|---|---|---|---|
| `one-env.ts` | 3 | `POST ${process.env.API_URL}/e/one` from the request summary; one graph row (client `post`, base `env`) | The base-path row states the fact's operation and gives way. Its graph row goes, and the fact's graph row reads its client and base. |
| `one-literal.ts` | 3 | `POST https://api.example.com/l/one` from the request summary; one graph row (client `post`, host `api.example.com`) | As above, for a literal origin. |
| `two-literal.ts` | 3 | `POST /t/a` and `/t/b`, both from the request summary; graph rows keep client `post`, host `api.example.com`, base none | Two calls on one line: each fact claims only its own key's rows (carrick#4433), so each base-path row gives way to its own fact. Only the row's source and role move. |
| `two-env.ts` | 3 | `POST /te/a` and `/te/b`, both from the request summary; graph rows keep client `post`, base `env` | As above, for an env origin. |
| `mixed.ts` | 3 | `POST /m/a` from the request summary | The `/m/a` base-path row gives way. `/m/b`'s fact stands with no graph host (R6). |
| `prefix-control.ts` | 3 | `POST /api/p/one` from the request summary | A path base is not an origin: no base-path row stands. |

The scan log's fold line reads
`kept_out={}`: every base-path row stated its own operation and gave way.
