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
  contract (`machinery_envelope`) and the pair is `unverifiable`.

Before the fix, both sites were typed as the whole object and judged
`incompatible`.

The model's answer for each file is in `model-cassette/analyze-file/<file
stem>.json`; `library-claims.json` is the store answer the scan replays.

Read by `tests/wrapper_request_line_test.rs`. It needs the built sidecar.

Protocols: HTTP only.
