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

## The answer key

| site | truth |
|---|---|
| `helpdesk.ts:7` | `POST ${process.env.HELPDESK_URL}/api/answer` |
| `helpdesk.ts:20` | `GET ${process.env.CATALOG_URL}/api/v1/items` |
| `toolset.ts:12` | `POST ${process.env.SERVICE_ASK_URL}/api/ask` |

All three env vars are undeclared in this fixture, so all three are unmatched
calls with an env-var base. That is the honest representation the schema already
has, and no match is claimed for any of them.

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

Line numbers in the cassette's `@line:` placeholders reference exact source
lines in `src/helpdesk.ts`. Re-count them after any edit to that file.
