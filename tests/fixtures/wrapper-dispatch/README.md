# `wrapper-dispatch`

Fixture for carrick#872: a body-dispatching route (carrick#831) called through
a client, where the value that says WHICH operation is asked for is written in
the client's body and the row that records the call is emitted at the site that
calls it.

## What it breaks that the other wrapper fixtures share

Every other wrapper fixture gives its client a route-shaped URL of its own
(`/api/v2/artifacts/${name}`, `${base}${path}`), so the request-member join can
assert a site's method and path from it. A client that talks to one RPC-style
route does not: it assembles the whole URL once, in the constructor, and keeps
it in a field. Every request in `src/api-client.ts` is `fetch(this.gatewayUrl,
…)`, which states no route-shaped argument at all, so the request-member index
drops all of them — and every consumer here would resolve to nothing if the
dispatch carry read that index. It reads its own, which asserts no target.

## The shape

- `src/api-client.ts` is the client. Its URL is built in the constructor and
  held in a field. Four members matter:
  - `searchByIntent` issues the request in its own body.
  - `getAllRepoData` issues none: it hands a callback to a cache, and the
    callback calls a sibling. The cache read is `this.cache.get(fn)` — a
    verb-named member call with no string argument, which is not a request.
  - `findService` is two delegations from the request (`findService` →
    `getAllRepoData` → `fetchCrossRepoData`).
  - `refreshEverything` issues TWO requests with two different actions. It is
    the control: a site calling it could be asking for either, so the source
    does not say, and nothing is carried.
- `src/cache.ts` holds the cache, so the delegation really does leave the
  client's file and come back.
- `fetchCrossRepoData` also reads a presigned URL with a bare `fetch(url)`. No
  options bag and no verb property, so it is not a second request and the
  member still reaches exactly one.
- `src/graph.ts`, `src/search.ts`, `src/services.ts` and `src/refresh.ts` are
  the consumers. Each takes the client as a PARAMETER and imports it type-only,
  which is the shape with no receiver constraint on the join.
- `src/local.ts` is the same-file half: a module-function wrapper called with
  its path as an argument, whose body writes the action literal. The site raises
  no candidate at all, so its row is the same-file wrapper pass's own
  (`resolution_source: same_file_wrapper`) and the carry stamps a value onto it.

## The answer key

| site | carried value |
|---|---|
| `search.ts:4` `client.searchByIntent(query)` | `action=search-by-intent` |
| `graph.ts:4` `client.getAllRepoData()` | `action=get-cross-repo-data` |
| `services.ts:4` `client.findService(name)` | `action=get-cross-repo-data` |
| `refresh.ts:4` `client.refreshEverything()` | none — two requests, no single answer |
| `local.ts:11` `postToGateway("/rpc/gateway", payload)` | `action=store-metadata` |

## The cassette

`__llm__/analyze-file/api-client.json` and `local.json` are the only ones that
state a `dispatch`, and they state it where the model really can: on the
client's own requests, beside the object literal that writes it. The client's
targets are `${this.gatewayUrl}`, which carries no literal path segment and is
dropped from the index downstream — the same reason the deployed scan recorded
zero edges for this shape and the delegating sites were the only record of the
calls.

The consumer cassettes hold what production holds: a row at the delegating site
with a method and a path and NO value, because the site's own file states no
literal for the model to read. Any dispatch on those rows is the scanner's join,
which is what `tests/wrapper_dispatch_test.rs` measures. No model behaviour is
being measured here.

Line numbers in the `@line:` placeholders reference exact source lines. Re-count
them after any edit to `src/api-client.ts` or `src/local.ts`.
