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
| `caught` | `.catch(() => null)`, a no-body path | `{ c: number; } \| null` | same |
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
| `seed-generic` | `res.data`, open `T`, the caller passes `Order[]` | `Order[]` | same |
| `seed-exact` | `res.data` (the labelled member) | `Order[]` | same |
| `seed-chained` | `const { data } = …; const { items } = data; return items` | `{ items: { id: string; }[]; }` | `Order[]` |
| `seed-widen` | `res.data \|\| null` | `string \| null` | `string` |
| `seed-widen-caught` | `res.data`, or `null` from a `catch` | `string \| null` | same |
| `seed-caught-void` | `.catch(() => {})`, a no-body path | `void \| { gone: boolean; }` | `{ gone: boolean; }` |
| `seed-caught-value` | `.catch(() => ({ rows: [] }))`, a value of its own | `{ rows: string[]; }` | same |
| `seed-envelope` | `{ body, response }` over an unlabelled `fetch`, body unstated | `unknown` | same |

`seed-same-file` (carrick#4513, the box's shape) declares a generic helper
and its callers in one file. The helper reads the JSON into its own `T`
(`const body: T = await res.json()`) and returns `{ status, body }`. The
model's row at the helper's own `fetch` (`src/seed-same-file.ts:2`) names no
call to bind `T`, so it states no type: `unknown`, `bound_by_callers`, never
`T` in the stored text (`bundled_types`). Before, the stored text was `T`.
Reading the body where a named caller holds it (`body` of the call's awaited
result) is P3 of carrick#4513, not yet built.

The `seed-*` rows are carrick#4513's. `seed-chained` holds a member of a
member, which no one key names, so the response is the labelled call's
body. `seed-widen` hands back the body or no body (`|| null`, `?? undefined`),
as a second `return null` would, so it is read at the deciding call, as
`seed-widen-caught` already was. A rejection handler that hands back no
body (`.catch(() => null)`, `() => undefined`, `() => {}`) is the same
no-body path (ruled 2026-11-17): `caught` and `seed-caught-void` keep the
null or void. One that hands back a value of its own stays at the
labelled call. `seed-generic`'s cassette names `T` as the
row's symbol, as a model reading the wrapper can.

## The review of carrick#4805 (round 2)

| Row | Wrapper hands back | Wanted response | Main | carrick#4805 before round 2 |
|---|---|---|---|---|
| `fb-coalesce` | `res.data ?? []` over `reply.get<any>`, declared `Promise<Order[]>` | `Order[]` | same | `any` (`declared`) |
| `fb-or` | `res.data \|\| []` | `Order[]` | same | `any` |
| `fb-if` | `if (!res.data) return []; return res.data;` | `Order[]` | same | `any` |
| `fb-tern` | `res.data ? res.data : []` | `Order[]` | same | `any` |
| `own-chain` | `isUp(p)`, which returns `r.v > 0` | `{ v: number; }` | `boolean` | `boolean` |
| `own-mapper` | `toDomain(res.data)`, an own mapper | `{ id: string; extra: number; }` | `{ id: string; }` | `{ id: string; }` |
| `own-maker-hop` | `makeGate(b)`, a maker, one hop down | `{ g: number; }` | `() => Promise<{ g: number; }>` | same as main |
| `pick-tern` | `c ? a(p) : b(p)`, two requests | `{ a: number; } \| { b: number; }` at both | same | each request's own body |
| `pick-two` | `if (c) return a(p); return b(p);` | `{ a: number; } \| { b: number; }` at both | same | same |
| `inline-data` | `(await reply.get(u)).data` | `{ q: number; }` | same | same |

A literal default beside the body is the body: the caller holds the body's
type or the default's, read at the deciding call. A callee's own value is
the wrapper's own too. Two requests in one conditional are not followed.

Mapping shapes keep main's read, the wrapper's own value; P3 and the
landing own them: `map-then` (`.then((r) => r.data.items)`, `string[]`),
`map-array` (`.data.map(fromDto)`, `number[]`), `map-new` (`new
User(data)`, `User`), `map-index` (`.data[0]`, `{ id: string; }`) and
`map-db` (a request, then a database read, `{ row: number; }`).

`keep-retried` (`retry(run)`), `then-data` (`.then(fn)`) and
`keep-local-run` (a local closure's call) end at a call the return pass does
not follow. That is undecided, so they are read at the deciding call, whole
(carrick#4513 P2).
