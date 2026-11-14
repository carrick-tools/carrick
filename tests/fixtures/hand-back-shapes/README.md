# hand-back-shapes

`web` calls `api` through wrappers of many shapes, each with no options of
its own, so the request summaries own every caller's row (carrick#4270). A
caller's value is the wrapper's own value. Only where that value can hold
the transport response beside the body it read does the compiler engine's
answer at the call name a body member, so only those callers are asked
there.

The wrappers are in `call.ts`, `more.ts` and `chain.ts` (`lib.ts` holds two
more). Their callers, and what each wrapper returns:

| Caller | Wrapper returns | Engine keeps | Asked |
|---|---|---|---|
| app.ts:14 | `{ res, json }` | yes, `implicit` | yes |
| app.ts:19 | two returns, `first` / `second` members | yes, `unknown` | yes |
| probe.ts:20 | `return callPlain(url)` | yes, `unknown` | yes (its callee holds) |
| lib-use.ts:6 | `return retry(() => callPlain(url))` | yes, `unknown` | yes (the callback's callee holds) |
| more-use.ts:4 | `return pack(res, json)` | yes, `unknown` | yes (a call's argument) |
| more-use.ts:9 | `return new Holder(res, json)` | yes, `unknown` | yes (a `new` argument) |
| more-use.ts:14 | `{ ...extra, res, json }` | yes, `implicit` | yes |
| more-use.ts:19 | `{ res: res.clone(), json }` | yes, `implicit` | yes |
| more-use.ts:24 | `{ res, json }`, `res` from `send`, which returns `fetch` whole | yes, `implicit` | yes |
| more-use.ts:30 | `{ res, json }` with a declared return type | yes, `implicit` | yes |
| more-use.ts:35 | `[res, json] as const` | yes, `unknown` | yes |
| chain-use.ts:9 | `fetch(u).then(async (res) => ({ res, json }))` | yes, `unknown` | yes (the callback's parameter) |
| chain-use.ts:4 | `fetch(u).then((r) => r.json())` | no | no |
| chain-use.ts:14 | `.then((r) => r.json()).catch(() => null)` | no | no |
| chain-use.ts:19 | `retry(() => fetch(u))` | no | no (a bare transport) |
| chain-use.ts:24 | `handle(res)`, which returns `{ ok, code }` | no | yes (a call's arguments are held) |
| probe.ts:4 | `return fetch(...)` | no | no |
| probe.ts:9 | `return res` | no | no |
| probe.ts:14 | `{ meta: { res }, json }` | no | yes (nested inside a member) |
| probe.ts:25 | `{ status: res.status, json }` | no | no |
| probe.ts:30 | `return json.data` | no | no |
| probe.ts:35 | `[res, json] as const`, read by member | no | yes (a tuple) |
| more-use.ts:40 | `{ headers: res.headers, json }` | no | no |
| lib-use.ts:11 | `{ ok: true, data }` | no | no |

So 15 callers are asked and 12 keep the answer. The three asked and not kept
are shapes the syntax cannot tell apart from a kept one. `app.ts:4` and
`app.ts:9` are model-owned rows of `hand-back-object`'s kind; they keep
their entries. Both pairs read `unverifiable`.

The model's answers are in `model-cassette/analyze-file/<file stem>.json`;
`library-claims.json` is the store answer the scan replays.

Read by `tests/wrapper_request_line_test.rs`. It needs the built sidecar.

Protocols: HTTP only.
