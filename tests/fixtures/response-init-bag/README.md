# response-init-bag

A call handed a path and an options object that holds only `headers` (and
`status`) (carrick#1986). A response's init takes exactly those keys, so
the object alone does not say whether the call sends a request or builds a
response. The fixture is scanned with the model off, so every row is one a
pass states as a fact.

One service, `web` (`apps/web`), beside a workspace package
`@fixture/http-kit` (`packages/http-kit`) whose source is in the
repository. `a-server-runtime` and `an-http-client` are declared
dependencies with no source in the repository.

## Answer key

| File | Line | Call | Row |
|---|---|---|---|
| security.server.ts | 10 | `return redirect(SETTINGS_PATH, { headers })`, a package export | none |
| security.server.ts | 17 | `throw redirect(LOGIN_PATH, { status, headers })` | none |
| settings.ts | 4, 7 | calls into the two helpers above | none |
| profile.ts | 19 | `await request(PROFILE_URL, { headers })`, a package export | `GET /api/profile` |
| profile.ts | 26 | `request(ORDERS_URL, { headers }).then(…)` | `GET /api/orders` |
| profile.ts | 32 | `sendJson(parser, PREFERENCES_URL, { headers })`, defined in the repository | `GET /api/preferences` |
| profile.ts | 38 | `fetch(NOTICES_URL, { headers })`, the platform's `fetch` | `GET /api/notices` |
| profile.ts | 42 | `return request(ACCOUNT_URL, { headers })`, a package export | none |

What decides it is what the source says about the call:

- **The callee.** The platform's `fetch` sends a request whatever its
  options hold. A function the repository defines is read from its source.
- **What is done with the result.** A request's result is waited on: it is
  awaited, or handed to `.then`, `.catch` or `.finally`. A response is
  returned or thrown.

Line 42 is a request, written exactly like line 10. Nothing in the source
tells them apart, so neither is stated as a fact; the model reads both.
