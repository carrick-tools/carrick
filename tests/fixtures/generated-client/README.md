# generated-client (fixture for carrick#2448)

One root service. A REST client written into the repository in the shape an
OpenAPI generator writes, in three rungs, plus negatives. The model answers
nothing (`__llm__/analyze-file/*.json` are empty), so every row is a
deterministic one. The only store label is the language library's `fetch`.

| File:line | Call | Main 14bdf1cc today | Wanted (ticket) |
|---|---|---|---|
| `r1/sdk.ts:5` | `(options?.client ?? client).get({ url: '/r1/items' })` | `request_spec` row only; lead `name_never_bound` at `r1/client.ts:10` | site `GET /r1/items`, end `fetch`, chain through the arrow at `r1/client.ts:10` (T1) |
| `r1/sdk.ts:8` | `(options.client ?? client).post({ url, body })` | `request_spec` only; lead at `r1/client.ts:11` | site `POST /r1/items` (T1; method from the arrow's own `fetch(…, { method: 'POST' })`) |
| `r1/sdk.ts:10` | `client.get({ url: '/r1/direct' })` | `request_spec` only; `unfollowed_member_calls` `interface_member` | site `GET /r1/direct`; the `interface_member` row goes (T1) |
| `r2/sdk.ts:5`, `:8` | `.get/.post({ url })` on a mapped-type client made by `makeMethodFn('GET')` | `request_spec` only; leads at `r2/client.ts:11` (`name_never_bound`), `:13` (`unanswered`) | sites `GET` / `POST /r2/items` (T1 + T2: the method the maker call binds) |
| `r3/sdk.ts:11`, `:18`, `:25` | the faithful generated shape: `{ url: '…', ...options }` into `request` → `beforeRequest` → `buildUrl` → `new Request` → `_fetch = opts.fetch!` (`client.ts:37`; `options.fetch ?? _config.fetch ?? globalThis.fetch` at `:22`); `opts` is handed to a config hook (`opts.requestValidator(opts)`) and to every interceptor, as the real shape does, so rule 2.3.3 makes its entries a hole | `request_spec` only; discovery reaches nothing | one fact row each, same method and path as today's `request_spec` row (`GET /api/v1/widgets`, `GET /api/v1/widgets/:id`, `POST /api/v1/widgets`) (T3 + T4) |
| `n1/cache.ts:19`, `:21` | the same call shape over a client that sends nothing | `request_spec` rows `GET` / `PUT /n1/cached` (wrong) | no row (T4's corroboration; slice F deletes the wrong rows) |
| `n2/jobs.ts:20` | `(options?.jobs ?? jobs).post({ url: '/n2/reports', ...options })` over a transport that publishes to a labelled broker with a topic no reader folds | `request_spec` row `POST /n2/reports` (wrong) | no HTTP row: the walk's end is a broker (T4's condition 2; its delete-check) |
| `n1/escaped.ts:11` | a client object written through (`loose.get = next`) | `request_spec` row `GET /n1/loose` | no site through the factory's arrow (T1's escape rule: a hole) |
| `app/use.ts` | callers of the SDK functions in another module | no rows (no model row for call-through to correct) | unchanged |
