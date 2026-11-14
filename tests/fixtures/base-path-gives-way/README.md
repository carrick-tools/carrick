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
| `two-literal.ts` | 3 | `POST /t/a` and `/t/b`, both from the base-path pass | Two calls on one line: the fold cannot tell which call each row belongs to, so both facts are kept out. |
| `two-env.ts` | 3 | `POST /te/a` and `/te/b`, both from the base-path pass | As above, for an env origin. |
| `mixed.ts` | 3 | `POST /m/a` from the request summary | The `/m/a` base-path row gives way. `/m/b`'s row is carrick#4433's and is not asserted. |
| `prefix-control.ts` | 3 | `POST /api/p/one` from the request summary | A path base is not an origin: no base-path row stands. |

The scan log's fold line reads
`kept_out={"env_base_path": 2, "literal_base_path": 2, "request_summary": 1}`.
