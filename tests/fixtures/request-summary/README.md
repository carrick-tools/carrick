# `request-summary`

Golden fixture for carrick#1555: calls made through a client whose request is
written one to three calls away from the line that makes the call.

It mirrors carrick-cloud's `lambdas/mcp-server` client, which is where the
shape was first reported (cloud#386, carrick#588, carrick#872):

- `src/api-client.ts` holds the gateway URL in a field the constructor
  assembles from an injected endpoint. Every request goes to that one route
  and names its operation in the body's `action` field. The file is longer
  than the 4 KB the model is shown of a wrapper module.
- `getAllRepoData` sends nothing itself: it hands a producer to
  `src/cache.ts`, which invokes it with an argument and chains the promise.
- `findService` is one delegation further out, `listProjects` goes through a
  private helper that serialises the body its caller writes, and
  `invalidateCache` sends nothing at all.
- The tools take the client as a parameter and import it only as a type.
  `src/tools/lookup.ts` names it `gateway`, so the candidate scanner raises
  nothing there.
- `startPolling` makes no call at all: it constructs a `Poller`
  (`src/poller.ts`) whose constructor sends a request. A summary cannot
  follow a construction, so it never proves the method sends nothing.
- `src/negatives.ts` holds calls shaped like requests that are not: a `get`
  on a `Map`, and a route registration.
- `findSimilar` writes its `action` BEFORE spreading the caller's params
  into the body, and `refresh` writes it AFTER. A spread of a value whose
  keys the source does not state may overwrite anything written before it,
  so only the second action is one the source states (carrick#1564 review,
  finding 1).

## The cassette

The model's answers are wrong the way production's were. The client's own
requests carry no dispatch. The consumers carry the verbs and actions the
model invented from method names: `GET` for a `POST`,
`get_all_repo_data`, `findService`, `invalidate_cache`. Nothing a test
asserts can come from the cassette: every value in the answer key below is
one the source writes.

## The answer key

| site | row |
|---|---|
| `api-client.ts:93` | `POST /types/check-or-upload {action=search-by-intent}` |
| `api-client.ts:103` | `POST /types/check-or-upload`, no action: the params spread after it may set one |
| `api-client.ts:113` | `POST /types/check-or-upload {action=analysis-job-status}` |
| `api-client.ts:129` | `POST /types/check-or-upload`, no action: the body is the caller's |
| `api-client.ts:147` | `POST /types/check-or-upload {action=get-cross-repo-data}` |
| `api-client.ts:163` | none: a presigned URL the source does not state |
| `api-client.ts:171` | `POST /types/check-or-upload {action=refresh}`: written after the spread |
| `tools/graph.ts:4` | `POST … {action=get-cross-repo-data}` |
| `tools/check-compat.ts:4` | `POST … {action=get-cross-repo-data}` |
| `tools/services.ts:4` | `POST … {action=get-cross-repo-data}` (two delegations) |
| `tools/find-similar.ts:4` | `POST …`, no action: the request line states none, and the model's `findSimilar` is never kept on a row the source states |
| `tools/projects.ts:4` | `POST … {action=list-projects}` (body through the helper) |
| `tools/lookup.ts:6` | `POST … {action=search-by-intent}` (no candidate) |
| `server.ts:9` | `POST … {action=analysis-job-status}` (inside a lambda) |
| `server.ts:12` | none: `invalidateCache` sends nothing, and the model's row is withdrawn |
| `server.ts:13` | `POST … {action=get-cross-repo-data}` |
| `server.ts:19` | the model's row, kept: `startPolling` constructs a `Poller`, whose constructor sends a request, so it is never proven to send nothing |
| `negatives.ts`, `tools/misc.ts` | none |
