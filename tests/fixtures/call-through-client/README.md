# call-through-client

Model rows at calls through the repository's own code (carrick#1146).

Four services:

- `packages/client` writes the requests: seven as request specs
  (`transport.get({ url: "/api/v1/runs/{id}" })`), one as a `fetch` with a
  literal path, one whose whole URL is its parameters, and two as a `fetch`
  on a declared environment base: one hands back its parsed body, the other
  wraps it in an envelope.
- `packages/cli` reaches them through a client class, a factory, and commands
  that call other commands.
- `apps/web` reaches them from hook arguments: a call spread into the hook's
  options, a callback among them, and a callback handed to one of the app's
  own functions.
- `services/api` serves the routes.

The cassette under `__llm__/` states what a model states at a call that writes
neither a method nor a path: a path made from the method's name, a base it
invented, a sibling's route, and one correct path. Rows whose id starts with
`model:` join no candidate, so they have no span and are placed by their line.

Rows that take the request their call reaches (`commands.ts` lines 9, 15, 21,
46, 64, 101, 145, 151, 157, 164, 176 and 186; `hooks.ts` lines 9, 14 and 19) carry the reached operation, the
source `request_summary`, the role `wrapper_call` and `reaches_request`. Line
21 is stated twice by the model and is one row afterwards. The correct row
(`commands.ts:28`) keeps its `model` source and its type entry, and gains the
role and the link.

Types at the corrected rows:

| Site | The type anchored there |
|---|---|
| `commands.ts:145` | `Run`, which matches what the API sends; the function called hands back the parsed body: kept under the reached operation and judged compatible |
| `commands.ts:157` | `SavedRun`, a stale copy that requires a field the API no longer sends: kept and judged incompatible |
| `commands.ts:9` | `Run`: the client method hands the request to a helper that reads `.data` of a request spec's result, one step: kept and judged compatible |
| `commands.ts:46` | `SavedRun`, the same chain through `save`, whose request the API answers without `savedBy`: kept and judged incompatible |
| `commands.ts:15` | no entry exists at the call (the caller declares no type), so none is kept |
| `commands.ts:21`, `:64`, `:101`, `:164` | the chain hands back the body, but the helper's `T` is never given, so the type is `unknown`: dropped and counted apart |
| `commands.ts:151` | anchored at a call whose function wraps the body in an envelope: dropped and counted |
| `commands.ts:176`, `:186` | the function called returns the transport's result whole, with no step read off it: dropped and counted |
| `hooks.ts:9`, `:14`, `:19` | anchored at a hook's or a runner's call, whose result is not what the request returns: dropped and counted |

A type is kept only where the chain of functions from the call's callee to the
request reads exactly one step off the transport's value: the platform's
`.json()` parse of a `fetch`, or one member read of a library client's result
(carrick#2377, `ReachGraph::hands_back_body` in `src/call_through.rs`). The
steps are counted over every function on the chain; a function whose value is
built into an object, passed through another function, or read twice leaves
the chain unproven.

`packages/cli/src/typed-commands.ts` states four rows, each at a client method
(`typed-client.ts`) that says what it returns:

| Site | Outcome |
|---|---|
| `typed-commands.ts:8` (`saveRun`) | `SavedRun` kept, incompatible: the helper chain of `commands.ts:46` |
| `typed-commands.ts:15` (`read`) | `Run` kept, compatible: a class member returning a function's parsed `fetch` |
| `typed-commands.ts:22` (`getRunView`) | dropped: the value goes through `toView` before it is returned |
| `typed-commands.ts:29` (`getRunState`) | dropped: two reads, the helper's member and then the run's own |

The calls in `typed-commands.ts` from line 36 have no model row. The requests
they reach are in `packages/client/src/typed.ts`: two specs (lines 13 and 19)
and a `fetch` on the declared base (line 24).

Where a call reaches more than one request, the request is the one whose value
the functions hand back. A request awaited and dropped, or whose value only
goes into the arguments of another request, is a side request
(`src/return_flow.rs`):

| Site | Side request | Reached |
|---|---|---|
| `commands.ts:64` | the client's health probe, a request a fact states | `GET /api/v1/runs/:id` |
| `commands.ts:164` | the client's health probe, a `fetch` only the model reads | `GET /api/v1/runs/:id` |
| `commands.ts:176` | the token handed to the request returned | `GET /api/v1/runs/:id` |

No corrected row carries a request type: a call's arguments are the called
function's parameters, not a request body.

Rows that stay as the model stated them:

| Site | Why |
|---|---|
| `commands.ts:57` | the helper called reaches two operations |
| `commands.ts:70` | nothing resolves the receiver |
| `commands.ts:76` | the function called writes its URL from its parameters, so no row states its request |
| `commands.ts:82` | the site writes the route |
| `commands.ts:88` | the site also writes a request of its own |
| `commands.ts:94` | only the model states the request the function called makes |
| `commands.ts:127` | the site makes a request itself, inside a call that reaches another |
| `commands.ts:139` | the function called asks for a token and returns a `fetch` only the model reads |
| `hooks.ts:24` | two callbacks, and nothing says which makes the request |

`commands.ts:113` writes its route into a function of the same file; the
scanner reads that row off the source and this pass never touches it.

`commands.ts:101` reaches a request the API serves under another verb, which
is the finding the pull request test reads.

## Calls the model stated no row at

A call into a function of its own service that another file declares, where
that function itself calls into another service and the walk from it reaches
exactly one request there, states that request at its own line, with the
source `request_summary`, the role `wrapper_call` and `reaches_request`,
whether or not the model stated a row there. With the cassette as it is, the
model stated nothing at these:

| Site | Reached |
|---|---|
| `commands.ts:34` | `GET /api/v1/token` |
| `commands.ts:40` | `PUT /api/v1/runs/:id` |
| `commands.ts:53` | `GET /api/v1/runs/:id` and `GET /api/v1/runs`: two calls, one row each |
| `commands.ts:121` | `POST /api/v1/audit` |

The second scan empties the answers for `commands.ts` and `hooks.ts` and
walks the compiler's rows (`CARRICK_POSITION_ROWS=1`); `packages/cli` has a
`tsconfig.json`, as a TypeScript package does, so the compiler resolves its
own modules' imports. Every call into the
client class then states its row from the source alone (`commands.ts` lines
9, 21, 28, 34, 40, 53, 64, 101, 121 and 164), and so does `tools.ts:9`, whose
`client` parameter only the compiler types (the table's type gives it one).
None is stated at a call one hop further up (`main.ts:5` calls a command of
another file, which calls the client; `commands.ts:15` and `:46` call a
command of the same file, `:57` a helper of the same file that reaches two
operations), or at a call nothing resolves (`commands.ts:70`). A call
straight into the client package is a different case, below.

`packages/cli/src/nested.ts`, with `helpers.ts` and `local-fetch.ts` beside
it, and `apps/web/src/page.tsx` with `badge.tsx`:

| Site | Rows |
|---|---|
| `nested.ts:7`, `:8` | the model states `GET /api/v1/status` at `Promise.all(` (line 7); the call into the client inside it (line 8) is that row's call and states none |
| `nested.ts:16` | one `GET /api/v1/status` row: two functions called on one line reach the one operation |
| `nested.ts:21` | none: `loadPair` reaches two operations |
| `nested.ts:27` | none: `localPing` reaches a request the CLI writes itself (`local-fetch.ts:5`) |
| `nested.ts:33` | none: `statusReader` writes a function that requests and hands it back unrun; only a function it calls, or hands to a call of its own, counts as what it runs |
| `nested.ts:39` | `GET /api/v1/status`: `statusViaReader` writes a reader and calls it |
| `page.tsx:5` | none: a JSX element is no call the source writes |
| `page.tsx:10` | `GET /api/v1/runs`: the same component called as a function |

## Calls straight into another service

A call into a function another service declares, where that function
reaches exactly one request written in that service, states the request at
the call (carrick#2773), whether or not the model stated a row there. The
function may hand its request back unrun: `getRunOptions` returns a `queryFn`
that a hook runs later.

- `apps/web` has a `tsconfig.json` that maps `@fixture/client` to the client
  package, so the compiler's rows read its calls. `packages/cli`'s maps
  nothing, and its calls are read as the scan reads an import the compiler
  cannot resolve. Both state the same kind of row.
- A call written inside a function of its own service that the service's other
  calls already reach the request through states nothing: those callers are
  the sites, one per module that calls in. So the client class's methods
  (`api-client.ts`), the probed clients' methods, `helpers.ts` lines 16 and 21
  and `badge.tsx:5` (called at `page.tsx:10`) state none.
- A function that no other file of the service calls, or that reaches two
  operations, or hands its request back unrun, is no wrapper, and its calls
  state theirs: `helpers.ts` lines 6 (two rows) and 27, and the probe in
  `probed-client.ts:14`.
- A function of the service's own file that is called from its own file only
  (`commands.ts:171`) is no wrapper either.
- The request must be written in the service that declares the function
  called. A function that reaches a request a third service writes states
  nothing (unit test in `src/call_through.rs`).

With the model's answers for `hooks.ts` and `commands.ts` emptied, the rows
the pass states are `hooks.ts` lines 9, 14, 19 and 24 and `commands.ts`
lines 145, 151, 157 and 171; `commands.ts` lines 76, 82 and 94 stay empty, for
the reasons in the table above.

Driven by `tests/call_through_rows_test.rs`.
