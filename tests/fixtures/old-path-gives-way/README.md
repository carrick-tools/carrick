# old-path-gives-way

Answer key for `tests/old_path_gives_way_test.rs` (carrick#4372). Every
call goes through a local wrapper in the same file, so the same-file
wrapper pass states a row at each call. `@fixture/http` is labelled in
`library-claims.json`, so the fact layer states a row at its calls too.
`@fixture/plain` carries no label, so its calls have no fact. The cases
are invented.

| File | Call line | What stands | Why |
|---|---|---|---|
| `two-lines.ts` | 3, 4 | `POST /z/a` and `POST /z/b`, each from the request summary, graph role `wrapper_call`, requests `1` and `2` | One call per line: the wrapper row states the fact's operation and gives way to it. |
| `same-twice.ts` | 3 | one `POST /w/a`, from the request summary, request `1` | Two identical calls on one line: the wrapper rows were the same row, keyed by place and operation. The second call's fact is kept out by the first's. |
| `two-same.ts` | 3 | `POST /x/a` and `POST /x/b`, both from the wrapper pass | Two calls on one line: each fact's call also holds a row of another operation, and the fold cannot tell which call that row belongs to. Both facts are kept out. |
| `two-wrappers.ts` | 4 | both wrapper rows, each with its own graph client, host and base (`sendA`, `a.example.test`, none; `sendB`, none, `env`) | As `two-same.ts`, through two wrappers. |
| `mixed-plain.ts` | 5 | `POST /q/a` and `POST /q/b`, both from the wrapper pass | `/q/b` goes to an unlabelled client and has no fact. `/q/a`'s fact is kept out, so the `/q/b` row is not lost. |
| `mixed.ts` | 4 | `POST /v/a` from the wrapper pass beside `GET /v/b` from the request summary | The line also holds the other call's summary row, of another operation, so `/v/a`'s wrapper row keeps its fact out. |
| `unlabelled.ts` | 3 | `POST /u/a` from the wrapper pass | No label, no fact. |

The scan log's fold line reads
`kept_out={"request_summary": 1, "same_file_wrapper": 6}`: six facts kept
out by a wrapper row (`two-same.ts` and `two-wrappers.ts` two each,
`mixed-plain.ts` and `mixed.ts` one each) and `same-twice.ts`'s second
call, kept out by the first call's fact.
