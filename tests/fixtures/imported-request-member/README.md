# `imported-request-member`

Fixture for carrick#588's wrong-method / wrong-version class: a call site whose
method and path live in a client module it imports, and nowhere in its own file.

## The shape

- `src/send.ts` is the transport. Its URL is the SECOND argument, so no
  candidate signal recognises a call to it as a request. Every request the
  client makes is therefore invisible to the candidate scanner.
- `src/apiClient.ts` is the client: one class, one method per endpoint, each
  stating its own verb and its own API version. It raises no HTTP candidate of
  its own, which is why the wrapper map's candidate gate is the wrong filter for
  reading its members.
- `src/artifacts.ts` is the consumer. It calls two of those methods, states
  neither a path nor a verb, imports the client type-only, and contains exactly
  one path-shaped literal: an error message naming `/api/v2/artifacts`, which
  belongs to neither call.
- `src/legacy.ts` exports a second binding whose method name collides with the
  client's and which issues no request. The consumer calls through it too, so
  the name join can be shown to be constrained by where the receiver was
  imported from rather than by the name alone.

## The answer key

| site | truth |
|---|---|
| `artifacts.ts:5` `client.createArtifactUrl(name)` | `PUT /api/v2/artifacts/:encoded` |
| `artifacts.ts:16` `client.readArtifactUrl(name)` | `GET /api/v1/artifacts/:encoded` |
| `artifacts.ts:21` `apiClient.createArtifactUrl(name)` | `GET /legacy/handles`, untouched |

## The cassette

`__llm__/analyze-file/artifacts.json` holds the WRONG answer on purpose: `POST`
for the upload (the verb guessed from the method name) and `/api/v2/artifacts` for
both (the error message's literal). That is what the deployed index recorded for
this shape before the fix. Freezing it makes `tests/imported_request_member_test.rs`
a regression net for the scanner machinery: any change to the resolution has to
show up as a change in the assertions, and no model behaviour is being measured.

Line numbers in the cassette's `@line:` placeholders reference exact source
lines in `src/artifacts.ts`. Re-count them after any edit to that file.
