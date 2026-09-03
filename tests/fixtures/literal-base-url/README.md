# `literal-base-url`

Fixture for carrick#627: a request whose URL interpolates a base declared once
as a module-level plain string literal.

## The shape

`src/checks.ts` holds two calls, both of the form
`fetch(`${BASE}/path`)` where `const BASE = "http://host:port"` sits at module
level. Sibling shapes already normalise: an env-var base resolves through the
alias map, a member-expression base (`${this.baseUrl}/…`) is carried as-is, and
an absolute host written at the call site needs no resolution at all. Only the
identifier-to-string-literal base was left with its `${BASE}` prefix in the
target, so the canonical key never reduced to the route path and the call
matched nothing.

## The answer key

| site | truth |
|---|---|
| `checks.ts:8` | `GET http://localhost:8080/status` |
| `checks.ts:10` | `GET http://localhost:3030/api/v1/whoami` |

Both hosts are local defaults that no service in this fixture serves, so both
are unmatched calls. What the fix changes is the path: `/status` and
`/api/v1/whoami`, rather than a target still carrying `${ADMIN_API}` and
`${APP_URL}`.

## The cassette

`__llm__/analyze-file/checks.json` holds the target verbatim, interpolation and
all, which is what the analyzer emits for this shape: the base is a binding it
cannot see the value of, and copying the source text is the honest answer.
Resolving it is the scanner's job, and freezing the cassette makes this a
regression net for that resolution rather than a measurement of the model.

Line numbers in the cassette's `@line:` placeholders reference exact source
lines in `src/checks.ts`. Re-count them after any edit to that file.
