# hand-back-object

A wrapper that hands back the transport response beside its parsed body
(`return { res, json: (await res.json()) as Reply }`) makes its call's value
that object, not the body (carrick#4185). The wrapper passes its options
through to `fetch`, so the summaries leave its callers to the model's rows,
and each caller's response type is the call's value.

- `app.ts:4` calls `call`, whose one return fills `json` from the body read:
  the caller's response type is that member, judged `compatible` against
  `api`.
- `app.ts:9` calls `callEither`, whose two returns fill different members
  from the body read: no member is decided, so the site states no response
  contract (`body_member_undecided`) and the pair is `unverifiable`.

- `app.ts:14` calls `callPlain`, which fetches with no options of its own,
  so the request summaries own the caller's row (carrick#4196). The
  compiler engine's answer at the call names the member it read (`json`),
  so the row takes the same response type as `app.ts:4` and the same
  verdict. A caller whose wrapper's answer names no member keeps no
  response type.

Before carrick#4185, `app.ts:4` and `app.ts:9` were typed as the whole object
and judged `incompatible`. Before carrick#4196, `app.ts:14` carried no
response type.

The model's answer for each file is in `model-cassette/analyze-file/<file
stem>.json`; `library-claims.json` is the store answer the scan replays.

Read by `tests/wrapper_request_line_test.rs`. It needs the built sidecar.

Protocols: HTTP only.
