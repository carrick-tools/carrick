# site-wrapper-response

Answer key for carrick#4371 (T1 and T5 in one change): where a wrapper
site's response is read. A wrapper's own value is never its response.

`@fixture/http` (in `node_modules/`) declares `post<T>`, `remove<T>` and
`purge<T>`, each resolving to the body. The store's claims
(`__llm__/library-claims/default.json`) label `post`'s result `whole: true`;
`remove` and `purge` carry no result label. `@fixture/reply` declares
`get<T>`, resolving to `Reply<T>`, and the store labels its result
`member: ["data"]`. The reader's cassette (`__llm__/analyze-file/`) states
one row per file.

Each wrapper is written in `src/<name>-lib.ts` and called at
`src/<name>.ts:2`. The deciding call is the caller's call of the wrapper;
the labelled call is the client call inside it.

The rule: the response is read at the deciding call only where the wrapper
hands back the labelled call's body, down a chain of returns, as the
store's result label says the body is read. Anywhere else it is read at the
labelled call, with the deciding call named so a type parameter the caller
binds is read as that call binds it (carrick#4375). A member read off a
call the store labels no result for is undecided, and stays at the
deciding call.

## Response at each caller (`src/<name>.ts:2`)

| Row | Wrapper hands back | Wanted response | Before |
|---|---|---|---|
| `own-member` | `res.ok` (`post`, labelled whole) | `{ ok: boolean; id: string; }` | `boolean` |
| `own-literal` | `true` | `{ id: string; }` | `boolean` |
| `own-nothing` | nothing | `{ purged: number; }` | `void` |
| `own-envelope` | `{ body, at: 1 }` | `{ ref: string; }` | `{ body: { ref: string; }; at: number; }` |
| `own-two-hop` | `r.v > 0` through a second wrapper | `{ v: number; }` | `boolean` |
| `own-based` | `res.hits > 0` | `{ hits: number; }` | `boolean` |
| `own-generic` | `res !== undefined`, open `T` | `{ alive: boolean; }` | `boolean` |
| `reply-status` | `res.status` | `{ name: string; }` | `number` |
| `reply-whole` | `reply.get(...)` whole | `{ city: string; }` | `Reply<{ city: string; }>` |
| `reply-generic-whole` | whole, open `T` | `{ size: number; }` | `Reply<{ size: number; }>` |
| `caught` | `.catch(() => null)` | `{ c: number; }` | `null \| { c: number; }` |
| `own-member-unlabelled` | `res.gone` (`purge`, no result label) | `boolean` (undecided: unchanged) | `boolean` |
| `keep-whole` | the request's value | `{ total: number; }` | same |
| `keep-awaited` | the request's value, awaited | `{ rows: number; }` | same |
| `keep-two-hop` | the request's value through a second wrapper | `{ depth: number; }` | same |
| `keep-generic` | the request's value, open `T` | `{ lines: number; }` | same |
| `reply-data` | `res.data` (the labelled member) | `{ name: string; }` | same |
| `reply-generic-data` | `res.data`, open `T` | `{ price: number; }` | same |
| `destructured` | `const { data } = …; return data` | `{ zip: string; }` | same |
| `keep-retried` | `retry(run)`, a closure handed on | `{ n: number; }` | same |
| `then-data` | `.then((r) => r.data)`, open `T` | `{ size: number; }` | same |
| `keep-local-run` | `run()`, a local closure | `{ done: boolean; }` | same |

`keep-retried`, `then-data` and `keep-local-run` move from the deciding call
to the labelled call; the labelled call holds the wrapper's open `T`, which
the deciding call binds (carrick#4375), so their types do not change.
