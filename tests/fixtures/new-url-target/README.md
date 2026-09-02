# `new-url-target`

Fixture for carrick#610: a request whose target is built as
`new URL(path, base)`, where the path literal is the constructor's first
argument and the base is opaque.

## The shape

`src/catalogue.ts` is one client with four requests.

- `createToken()` at line 13 is the decoy. It states its own path inline, on
  the retired API version, which is what makes `/api/v1/...` a plausible thing
  for extraction to reach for on the three sites below. It is also the control:
  the rule must leave a target the model stated correctly exactly as written,
  base and all.
- `listThings()` at line 18 is the direct form: the URL object is the request's
  own argument.
- `findThings()` at line 27 is the binding form, read back through `.href`.
  This is how a URL object nearly always reaches a request, because the search
  params get appended in between.
- `archiveThing()` at line 33 is the template form: a literal head with an
  interpolated segment behind it.

## The answer key

| site | truth |
|---|---|
| `catalogue.ts:13` | `POST ${this.baseUrl}/api/v1/token` |
| `catalogue.ts:18` | `GET /api/v2/things` |
| `catalogue.ts:27` | `GET /api/v2/things/search` |
| `catalogue.ts:33` | `POST /api/v2/things/${id}/archive`, keyed `/api/v2/things/:id/archive` |

The three `new URL` sites carry a path and no base. That is deliberate: the
base is a field here, and asserting a host the source does not state is the
defect this fixture exists for. A host-free call matches by route path, which
is what every other baseless call in the index does.

## The cassette

`__llm__/analyze-file/catalogue.json` holds the answer the deployed index
recorded for this shape. The two sites that build the URL a statement earlier
get the binding, which is all extraction can honestly read off `fetch(url.href,
…)` and `fetch(url, …)`, and a bare binding is not route-shaped, so both were
dropped before they became a row of any kind. The direct site gets the wrong
API version, lifted from the neighbouring method, which is the wrong answer the
deployed index recorded for this class. Freezing all three makes
`tests/new_url_target_test.rs` a regression net for the scanner machinery: no
model behaviour is being measured.

Line numbers in the cassette's `@line:` placeholders reference exact source
lines in `src/catalogue.ts`. Re-count them after any edit to that file.
