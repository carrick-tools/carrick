# catch-all-type-pair

Synthetic two-service workspace for carrick#4186. `http-server` is a
hand-written declaration, not a real package.

- `gateway/src/server.ts` registers the catch-all `ALL /v1/*` (line 14) and the
  concrete `POST /v1/jobs/open` (line 18).
- `worker/src/jobs.ts` calls `POST /v1/jobs/close` (line 15), which only the
  catch-all serves, and `POST /v1/jobs/open` (line 21), whose body type
  disagrees with the route's.

The catch-all's wildcard covers the literal segments of the first call, so it
says nothing about that path's contract: the edge is stored and the type check
compares nothing. The second call is a real break and keeps its verdict.

`expected.json` is the answer key and `__llm__/` holds the mocked model
answers for a `CARRICK_MOCK_ALL` scan. Used by `tests/catch_all_type_pair_test.rs`.
