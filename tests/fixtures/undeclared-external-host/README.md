# `undeclared-external-host`

Fixture for carrick-cloud#656: a call written as an absolute URL literal whose
host nobody declared.

## The shape

Two calls, identical in every way except their origin, and **no `carrick.json`
at all** — which is the point. `externalDomains` is empty in every carrick.json
until someone fills it in, and that is the state a first scan of any repo runs
in.

- `src/mailer.ts:4` calls `https://api.example-mail.test/emails`. A third party.
  Its origin is the only evidence the call leaves the system, so the match key
  keeps it. Stripped, the key is `/emails`, which reads exactly like an internal
  call to a producer nobody declares — and the reader reports a working
  third-party API as a missing endpoint.
- `src/selfcall.ts:5` calls `http://localhost:7100/emails`. The same path, over
  loopback. That origin is this machine and classifies nothing, so it is
  stripped and the key is the bare path, which is what lets a service's
  self-call match its own endpoint.

The paths are deliberately the same. What separates the two keys is the origin
and nothing else.

## The answer key

| site | `key` |
|---|---|
| `src/mailer.ts:4` | `http\|POST\|https://api.example-mail.test/emails` |
| `src/selfcall.ts:5` | `http\|POST\|/emails` |

`api.example-mail.test` is a reserved-TLD name, so the fixture names no real
third party and resolves nowhere.

## The cassette

`__llm__/` holds what extraction says about each call: the URL the call site
passes, whole. Both rows are the model's — a bare absolute literal states no
role, so no deterministic source emits one — which is exactly the path the
defect was on.
