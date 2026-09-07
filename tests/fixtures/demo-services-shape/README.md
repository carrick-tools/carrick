# `demo-services-shape`

Fixture for carrick#732 and carrick#733: the two shapes a three-service demo is
written in, reduced to one repo. A decorator-routed producer, a consumer whose
origin comes from the environment and whose path is written at the call site,
and the decoys that must stay silent.

Both shapes reached the index as the model's own reading before these tickets,
so a mismatch between them was a candidate rather than a fact — see
carrick#727 for what that costs a reader, and carrick-cloud#614 for the ruling
that named these two passes as the honest fix.

## The shapes

`src/users.controller.ts` is the producer. The class carries the prefix, each
method carries a verb and a path:

- `@Get()` with no argument at all: the route IS the prefix (`GET /api/users`).
- `@Get(":id")` and `@Post(":id/rename")`: the path below the prefix.
- `@ApiTags("people")` sits beside `@Controller("api/users")` and also takes one
  string. It is imported from a different module than the verbs, which is what
  singles the routing decorator out — structurally, with no package named.
- `@Trace()` is an HTTP method by the letter of the spec and is not a route:
  the verbs read here are the seven a handler is realistically named after, the
  same list the controller-method rule already uses.
- `nextId()` carries no decorator and is not a route.

`src/orders.ts` is the consumer. `USER_SERVICE_URL` is read from the
environment with a local default, and each call writes its own path:

- a template literal with a path parameter (`/api/users/${userId}`);
- the same with a longer path, on a verb-named `post`;
- the same statement written as a concatenation (`USER_SERVICE_URL + "/api/users"`);
- the environment read at the call site, with no binding in between.

`src/framework.ts` and `src/docs.ts` stand in for whatever libraries a service
uses. Their names carry no weight: the scanner reads the shape of the
decorators. `docs.ts` exists so a decorator that also takes one string, from
somewhere else, is present to be told apart from the routing one, and
`observability.ts` so a verb-named decorator that is not a route is.

## The decoys

`src/decoys.ts` must produce no row at all:

| line | shape | why it is silent |
|---|---|---|
| 8 | `axios.get("/api/users/1")` | a bare path literal with no base. Whether it registers a route or requests one is not a structural fact, and no rule here may guess (ruling, 2026-09-05) |
| 18 | `` axios.get(`${LITERAL_BASE}/api/things`) `` | the base is a string literal backed by nothing but its own initialiser, not the environment (carrick#627/#641 own that shape) |
| 27 | `` axios.get(`${this.opts.lookupUrl}/api/lookup`) `` | an injected base the file cannot see behind |
| 36 | `class Unprefixed` with `@Get("orphan")` | the declaration states no prefix, and reading one here would make every undecorated class a routing claim |
| 48 | `class Tagged` with only `@ApiTags("people")` | the only string on the declaration comes from a module that supplies no verb. A routing decorator taking no argument leaves the prefix implicit, and reading the tag's string instead would put `/people/:id` — a path nothing serves — into the index as a FACT |

## The answer key

| site | row | source |
|---|---|---|
| `users.controller.ts:20` | `GET /api/users` (`list`) | `decorator_route` |
| `users.controller.ts:25` | `GET /api/users/:id` (`find`) | `decorator_route` |
| `users.controller.ts:30` | `POST /api/users/:id/rename` (`rename`) | `decorator_route` |
| `orders.ts:9` | `GET ${process.env.USER_SERVICE_URL}/api/users/${userId}` | `env_base_path` |
| `orders.ts:14` | `POST ${process.env.USER_SERVICE_URL}/api/users/${userId}/rename` | `env_base_path` |
| `orders.ts:23` | `GET ${process.env.USER_SERVICE_URL}/api/users` | `env_base_path` |
| `orders.ts:29` | `GET ${process.env.USER_SERVICE_URL}/api/health` | `env_base_path` |

The route line is the method's NAME, not the decorator above it: the method's
span opens at its first decorator, and a reader following the index wants the
handler. When the model also answers for a route, its row folds onto the
deterministic one and is placed at the candidate it answered at — the decorator
line — while keeping `decorator_route` as the source (the carrick#712 rule).

Each call's target is emitted in the SOURCE's own spelling
(`${USER_SERVICE_URL}/api/users/${userId}`); the existing base-resolution pass
rewrites the base to `${process.env.…}` afterwards, exactly as it rewrites the
model's reading of the same line. That is why these rows key and match as they
always did, and only their stated source changes.

`USER_SERVICE_URL` is deliberately NOT declared in a `carrick.json` here, so
every call keeps its env-var origin in the key and matches no endpoint. An
undeclared env-var call is an expected find, never something a fixture
pre-declares away.

## No cassette

There is no `__llm__/` directory: every test over this fixture supplies its own
cassette. `deterministic_emission_test` runs it three ways — a model that
answers nothing, a model that contradicts every row, and a model that answers
for a route the decorators already state.
