# `env-var-whole-url`

Fixture for carrick#572: a request whose WHOLE URL is read from an environment
variable, passed to `fetch` as a binding.

## The shape

`src/helpdesk.ts` holds two calls.

- `askHelpdesk()` at line 7 does `fetch(url, …)` where
  `const url = process.env.HELPDESK_URL ?? "http://localhost:7100/api/answer"`.
  The call site states no path. The only path this request has anywhere in the
  source is inside the fallback literal.
- `listItems()` at line 20 is the base-plus-path shape that already resolved:
  `fetch(`${base}/api/v1/items`, …)`. It is here so the new rule can be shown
  not to disturb it.

`src/toolset.ts` holds one more, for carrick#632. Same binding shape, but the
request sits in an arrow function that is a property of an object literal handed
to a factory call, and nothing in the file states a path outside the fallback
literal. Its cassette holds no row for the site, so the row exists only if the
scanner emits it itself.

`src/answered.ts` holds the same buried shape at a site the analyzer DOES answer
for, paraphrasing the binding as the bare env-var name. That is what the live
index held for the call carrick#632 was filed from, and it is the case the #633
emission never reached: the row covered the site, so nothing was emitted, and
the call stayed keyed on an env-var origin.

## The answer key

| site | target | key |
|---|---|---|
| `helpdesk.ts:7` | `POST ${process.env.HELPDESK_URL}/api/answer` | `/api/answer` |
| `helpdesk.ts:20` | `GET ${process.env.CATALOG_URL}/api/v1/items` | `${process.env.CATALOG_URL}/api/v1/items` |
| `toolset.ts:12` | `POST ${process.env.SERVICE_ASK_URL}/api/ask` | `/api/ask` |
| `answered.ts:11` | `POST ${process.env.KNOWLEDGE_URL}/api/lookup` | `/api/lookup` |

Every env var here is undeclared, so every row is an unmatched call and no match
is claimed for any of them. The three whole-URL calls key on the route they
request, because their fallback states a loopback origin and that is a
structural prefix — the same key `fetch("http://localhost:7100/api/answer")`
already gets. `helpdesk.ts:20` states its own path behind an opaque base, so
nothing states its origin and the key stays verbatim.

## The cassette

`__llm__/analyze-file/helpdesk.json` holds `"target": "url"` for the first call,
which is all extraction can honestly say about `fetch(url, …)`. A bare
identifier is not route-shaped, so before the fix the row was dropped in
`build_mount_graph` before a `DataFetchingCall` existed, and nothing downstream
could see it. The projection carried one call, not two.

`__llm__/analyze-file/toolset.json` holds no answer at all: an empty
`data_calls` array, which is what the analyzer returns for a request buried in a
factory-call argument whose file states no path anywhere. That is the shape
carrick#632 was filed for, where the resolution had nothing to rewrite and the
call was absent from the index rather than merely wrong. The file must exist even
though it is empty: with no cassette for a file the mock falls back to a
schema-generated response, and the test would stop measuring the scanner.

`__llm__/analyze-file/answered.json` holds the answer the live index carried for
this shape: the right line and the right method, with the binding paraphrased as
`${KNOWLEDGE_URL}/api/lookup`. Nothing about it is a hallucination — it is simply
not the spelling the pipeline resolves env vars through, and it carries nothing
about the origin the source defaults to.

Line numbers in each cassette's `@line:` placeholders reference exact source
lines in the file it answers for. Re-count them after any edit to those files.
