# env-var-template-call-segment

carrick#829: a request whose path interpolates a CALL expression in one of its
segments is recorded, instead of vanishing for want of a route shape.

`src/ingest.ts` makes two requests through the same env-var base:

| line | target | what it is |
|---|---|---|
| 10 | `` `${INGEST_BASE}/v1/ingest/${encodeURIComponent(DATASET)}` `` | the missed shape: a call expression inside a placeholder |
| 24 | `` `${INGEST_BASE}/v1/status/${region}` `` | the control: an identifier inside a placeholder |

Both are one env-var base and one interpolated path parameter. The only
difference is what stands inside the `${…}`, and before carrick#829 that
difference decided whether the call became a row at all: `is_valid_route_shape`
rejected any route containing a parenthesis, a rule written to catch leftover
JavaScript source (`a || b`, a bare call expression standing where a route
should be) and applied to the whole target, placeholders included.

Nothing is declared: there is no `carrick.json`, which is the state a first
scan of any repo runs in, and an undeclared env-var base is an expected find
rather than something a fixture pre-declares.

The LLM is replayed from `__llm__/`, and the cassette holds what extraction can
honestly say about each call: the target verbatim, as written.
