# xrepo-corpus-3 — messy-realism cross-repo accuracy corpus

Seven statically-analyzable (not runnable) repos with an owned answer key, for the
cross-repo accuracy eval. Corpus-1 is the request/response corpus, corpus-2 the
event-driven dual; corpus-3 is the **messy-realism** corpus: breadth, indirection
and type depth **within the four shipped protocol families** (HTTP / GraphQL /
socket / pubsub), on the axes neither earlier corpus exercises. No new protocol
family is introduced (gRPC/tRPC are deliberately out of scope — new-machinery
programs of their own). Labels are **spec-of-record**: authored first, code built
to match, never derived from a scan. Change a label only by hand, in a separate
commit, reviewed against the spec — never to match scanner output.

Selected via `CARRICK_EVAL_CORPUS=xrepo-corpus-3` through the same two-phase scorer
(`tests/eval_xrepo.rs`); default (`xrepo-corpus-1`) is unaffected.

## Why this corpus exists (the gap list it covers)

1. **Broker-agnostic pub/sub, proven** — two brokers never seen by corpus-2
   (RabbitMQ via `amqplib`, BullMQ) plus the first **cross-broker edge** (NATS
   publisher → Kafka subscriber on one topic; the `pubsub|<topic>` key is
   broker-agnostic by design, until now untested) and the first **fan-out**
   (one publisher, two subscriber repos on `catalog.price.updated`).
2. **Consumer indirection** — a hand-rolled `apiClient` wrapper over `fetch`
   (base URL applied inside the wrapper, call sites carry only relative paths +
   a call-site generic), env bases read through a **config object**
   (`config.ordersApiUrl`) instead of inline `process.env`, and two client
   libraries new to the corpora (`got`, `ky`).
3. **Type depth** — contract types derived with **`zod` `z.infer`** (zod was a
   phantom dep in corpus-1; here it is used in code, and is also the dep-conflict
   vehicle: 3.23.0 vs 4.0.0, both sides real usage), a **shared workspace types
   package** (`@meridian/contracts`) imported across monorepo package boundaries,
   an HTTP **array response** (`TimelineEvent[]`), and a `DELETE` → 204 no-body
   endpoint (anchor must stay null).
4. **New compat-mismatch axes** — `Date` vs ISO-string, array-vs-scalar,
   `T | null` vs optional (`?: T`), and missing-required-field; plus two subtle
   **compatible** true negatives (producer-extra-fields narrowing; required →
   optional narrowing on the subscriber side).
5. **New HTTP shapes** — Koa (`@koa/router`, incl. the `router.del` alias),
   `PATCH`, versioned paths with a **version-drift negative** (consumer on
   `/api/v1/...`, producer serves only `/api/v2/...` — must NOT match),
   multi-segment nested params (`/products/:id/variants/:variantId`), first
   **matched** GraphQL mutation, first **CLIENT->SERVER** socket edge, first
   `gql`-tag consumer with **no call-site generic** (exercises the #298
   file-analyzer hint path).
6. **New decoy families** — supertest test-client calls, msw mock handlers,
   `amqplib` `assertQueue` topology setup.
7. **A call to the service's own route** — a job that fetches its own
   service's endpoint over a loopback origin. It is a consumer of that route:
   an expected call row marked `own_route`, and the corpus's one edge whose
   producer and consumer are the same service (carrick#1926). Until that was
   ruled it was labelled a decoy.

## Repos

| Repo | Stack | Role in the graph |
|---|---|---|
| `platform-monorepo/` | npm workspaces: `@meridian/contracts` (zod schemas + shared types) + `catalog-api` (Koa + `@koa/router` + nats) | HTTP producer (v2 products/variants; PATCH; DELETE-204), NATS fan-out publisher `catalog.price.updated`, supertest decoy |
| `orders-api/` | Fastify + amqplib + bullmq + kafkajs + @aws-sdk/client-sqs | HTTP producer (`POST /orders`, `GET /orders/:orderId/timeline`), RabbitMQ publisher `inventory.stock.adjust`, BullMQ publisher `shipments.dispatch`, Kafka subscriber `orders.status.changed` (cross-broker), SQS publisher (roadmap) |
| `inventory-svc/` | Express + amqplib + nats + zod@3 | RabbitMQ subscriber `inventory.stock.adjust` (payload type via `z.infer`), NATS subscriber `catalog.price.updated` (fan-out A), HTTP producer called by its own nightly job (the same-service edge), assertQueue decoy |
| `fulfillment-worker/` | bullmq + nats + got (no HTTP framework) | BullMQ Worker `shipments.dispatch` (subscriber=producer), NATS publisher `orders.status.changed`, `got` consumer of catalog variants |
| `storefront-web/` | Next-ish + ky + graphql-tag + socket.io-client + msw + zod@4 | `ky` consumer `POST /orders`, v1 drift consumer, gql query consumer (no generic → #298), socket emitter `support:message`, msw decoy |
| `ops-console/` | plain node BFF: fetch-wrapper + graphql-request + nats | wrapper consumers (PATCH products, GET timeline) through config-object env bases, gql mutation consumer (with generic), NATS subscriber `catalog.price.updated` (fan-out B) |
| `support-desk/` | NestJS + SDL GraphQL + socket.io | SDL producer (query/mutation/subscription), socket listener `support:message` (CLIENT->SERVER), Implicit-inference HTTP orphan |

## Configuration

Consumer repos with `${ENV}`-based HTTP calls carry `carrick.json` `internalEnvVars`:
`fulfillment-worker` (`CATALOG_URL`), `storefront-web` (`NEXT_PUBLIC_ORDERS_API_URL`,
`NEXT_PUBLIC_CATALOG_URL`), `ops-console` (`ORDERS_API_URL`, `CATALOG_URL` — note
these are read through `src/config.ts`'s config object, not inline at the call
site). `platform-monorepo/carrick.json` is the multi-service descriptor (only
`catalog-api` is a service; `contracts` is a library resolved via tsconfig paths).
Pub/sub, GraphQL and socket edges need no connection-classification config (they
key on topic/operation/event identity, not URLs).

## Cross-repo edges

Producer/consumer direction follows the standing model: HTTP/GraphQL producer =
the server; socket **listener** = producer; pub/sub **subscriber** = producer
(publisher/emitter = consumer). All tiers `capability` unless marked.

| # | Protocol | Producer | Consumer | Key | Compat |
|---|---|---|---|---|---|
| 1 | http | catalog-api `GET /api/v2/products/:id/variants/:variantId` | fulfillment-worker (got) | `http\|GET\|/api/v2/products/:param/variants/:param` | compatible (consumer omits `productId` — producer-extra-fields narrowing) |
| 2 | http | catalog-api `PATCH /api/v2/products/:id` | ops-console (wrapper) | `http\|PATCH\|/api/v2/products/:param` | **incompatible** (producer `price: {…} \| null` vs consumer `price?: {…}` — null is not undefined) |
| 3 | http | orders-api `POST /orders` | storefront-web (ky) | `http\|POST\|/orders` | compatible |
| 4 | http | orders-api `GET /orders/:orderId/timeline` | ops-console (wrapper) | `http\|GET\|/orders/:param/timeline` | **incompatible** (producer `TimelineEvent[]` vs consumer scalar `TimelineEntry` — array-vs-scalar) |
| 5 | pubsub/rabbitmq | inventory-svc (amqplib consume) | orders-api (sendToQueue) | `pubsub\|inventory.stock.adjust` | compatible |
| 6 | pubsub/bullmq | fulfillment-worker (Worker) | orders-api (Queue.add) | `pubsub\|shipments.dispatch` | **incompatible** (subscriber `dispatchAfter: Date` vs publisher ISO `string` — date serialization) |
| 7 | pubsub/**cross-broker** | orders-api (kafkajs subscribe) | fulfillment-worker (nats publish) | `pubsub\|orders.status.changed` | compatible |
| 8 | pubsub/nats | inventory-svc (subscribe) | catalog-api (publish) | `pubsub\|catalog.price.updated` | compatible |
| 9 | pubsub/nats | ops-console (subscribe) | catalog-api (publish) | `pubsub\|catalog.price.updated` | compatible (subscriber `effectiveAt?` optional vs publisher required — safe narrowing) |
| 10 | graphql | support-desk `query ticket` | storefront-web (gql tag, **no generic**) | `graphql\|query\|ticket` | compatible |
| 11 | graphql | support-desk `mutation escalateTicket` | ops-console (`request<T>`) | `graphql\|mutation\|escalateTicket` | **incompatible** (consumer requires `assignee: string`; producer `EscalationResult` lacks it) |
| 12 | socket | support-desk (server `socket.on`) | storefront-web (client emit) | `socket\|CLIENT->SERVER\|support:message` | compatible |
| 13 | http | inventory-svc `GET /warehouses/:warehouseId/stock/:sku` | inventory-svc (`fetch` over loopback, `src/jobs/reindex.ts`) | `http\|GET\|/warehouses/:param/stock/:param` | unlabelled (see below) |

**Edge 13 is the same-service edge**: `inventory-svc` calls its own route, so
the edge's producer and consumer are one repo, and the call row in
`inventory-svc/expected.json` is labelled `own_route`. Its `type_compatible` is
`null`, the one edge here without a verdict label. The type check pairs a
service with itself (carrick#1945), but the job reads nothing of the response,
so there is no contract to compare: the pair is unverifiable, which states no
`type_compatible`, and the edge is skipped on the compat row.

**Edges 8/9 are the fan-out**: one publisher (catalog-api), two subscriber repos —
two producers on one key (`pubsub|catalog.price.updated`), so two match edges
sharing a consumer. **Edge 7 is the cross-broker edge**: the publisher moved to
NATS mid-migration while the subscriber still reads the mirrored Kafka topic; the
key is broker-agnostic so they must match.

### Version-drift negative (must NOT match)
storefront-web calls `GET ${NEXT_PUBLIC_CATALOG_URL}/api/v1/products/:id`;
catalog-api serves only `/api/v2/products/:id`. Both sides are labelled orphans.
A match between them is a false positive.

## Orphans

Producers: catalog-api `GET /api/v2/products/:id` (drift counterpart) and
`DELETE /api/v2/products/:id` (204, no body → null anchor, null resolved type);
support-desk `GET /tickets/:id` (**Implicit**: no return annotation, inferred
`{ id: string; subject: string; ageDays: number; }`) and
`graphql subscription ticketUpdated`. inventory-svc
`GET /warehouses/:warehouseId/stock/:sku` is not one: its own job calls it
(edge 13).
Consumers: storefront-web `GET /api/v1/products/:id` (drift);
orders-api `pubsub|notifications.digest` (**roadmap**, see Tiers).

## Tiers

Everything is `capability` except the SQS digest publisher
(`orders-api/src/mq/digest.publisher.ts`): its queue identity lives in a URL
template (`${SQS_BASE_URL}/notifications.digest`), i.e. an **env-templated
topic** — extraction of these is deferred (`src/engine/mod.rs` env-template
note). It is tracked as a `roadmap` orphan consumer with key
`pubsub|notifications.digest`, flipped to `capability` only when that extraction
lands — never to inflate the score.

## Decoys (anti-overfit traps)

Formal `_must_not_emit` entries (HTTP-keyed, scoreable — paths chosen to collide
with NO legit op corpus-wide, since the decoy pool is matched against every
repo's emissions):
1. `platform-monorepo/packages/catalog-api/test/products.test.ts` — supertest
   `request(app).get("/api/v2/health")`: a **test-client call**, not a runtime
   consumer. `{kind: "call", GET /api/v2/health}`.
2. `storefront-web/mocks/handlers.ts` — msw `http.get("/api/v2/promotions/:id")`:
   a **mock route registration**, not a producer. `{kind: "endpoint", GET
   /api/v2/promotions/:id}`.
3. Documented, not formally scoreable (non-HTTP; same limitation corpus-2 logs):
   `inventory-svc/src/mq/setup.ts` `ch.assertQueue("inventory.stock.adjust")` —
   queue **topology setup**, not subscribe; and the SQS `sqs.send(…)` must not be
   emitted as an HTTP call.

## Type-inference cases

- **`z.infer` contracts**: `@meridian/contracts` (`Price`, `VariantDetail`) and
  inventory-svc's `StockAdjustCommand` are zod-schema-derived; the sidecar must
  expand through the zod stub's mapped type.
- **Shared workspace package**: catalog-api imports `Product`/`VariantDetail`
  from `@meridian/contracts` via tsconfig paths across the package boundary.
- **Array response**: `GET /orders/:orderId/timeline` resolves to an inlined
  array form (`{ … }[]`).
- **No-body response**: `DELETE /api/v2/products/:id` → anchor null, resolved null.
- **Implicit**: support-desk `GET /tickets/:id` has no return annotation.
- **Explicit everywhere else.**

## Dependency conflict

One deliberate cross-repo conflict: `zod` **3.23.0** (platform-monorepo +
inventory-svc) vs **4.0.0** (storefront-web) → major-incompatible → `critical`.
Unlike corpus-1's phantom zod, both sides use zod in code.

## Protocol detectability notes

- RabbitMQ/BullMQ have no scanner-side adapter — extraction rides the
  broker-agnostic `pubsub_operations` file-analyzer schema (shape-based, "any
  messaging client"). BullMQ trap: the topic is the **queue name**
  (`shipments.dispatch`), not the `Queue.add` job-name argument (`"dispatch"`).
- The socket edge uses ESM `socket.io`/`socket.io-client` imports (the CJS
  `require` form is a known blind spot, deliberately not used).
- The wrapper-client call sites (`ops-console`) carry only relative paths; the
  post-#294 canonical key (host-free) is what makes them matchable.
