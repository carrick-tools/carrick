# pr-seeds: one seeded PR change per finding kind and per protocol

Three repos and the contracts between them, used by the `seed_*` tests of
`tests/pr_on_main_test.rs` (carrick#3095). Each seed edits one repo on a branch
and runs a real PR scan against main's stored index. The tests replay the
model from hand-written answers in each repo's `__llm__/`, and run with the
five fact-layer switches the cloud runner sets.

- `orders-api` serves an HTTP route (`app/routes/api.v1.orders.$orderId.ts`),
  a GraphQL schema and resolver (`src/schema.graphql`, `src/resolvers.ts`), a
  socket listener and one emit (`src/realtime.ts`), and a pub/sub subscriber
  and publisher (`src/events.ts`).
- `storefront` is the other side of each. Seeds never edit it, so every break
  is a consumer the PR did not update.
- `admin-api` registers routes in code through a router stub, for two
  negatives. It scans against a store of its own: its stub pins a package no
  registry has, which would make every cross-repo type check in a shared store
  unverifiable.

This tree is an answer key. Do not edit it, or a seed's edit, to make a run
pass. A seed that fails today is `#[ignore]` with the issue that fixes it; that
issue removes the ignore.

Traps: a file with no model answer gets a generated one that can invent rows,
so every registration file has an answer; a seed that moves lines in a file
with an answer moves the answer's lines too; `node_modules/` stubs are
gitignored and added with `git add -f`; `CARRICK_MOCK_FIXTURE_DIR` must end
in `/`.

## Seeds

Names are `seed_<name>` with `-` as `_`. `_marking` tests hold the part of a
seed that is dark today. "Ignored" names the issues it waits on.

| Seed | Edit | Asserts | State |
| :--- | :--- | :--- | :--- |
| `http-field-rename` | route type and value, `total` to `amount` (route file :5, :14) | `type_mismatch` GET `/api/v1/orders/:orderId` at `src/checkout.ts:4` and `src/orders-client.ts:14`, `on_main: false`, `edge_source: fact`, `resolved`, no delta | passes |
| `http-field-removal` | route drops `currency` | the same two rows | passes |
| `http-verb-change` | `loader` to `action` (route file :9) | `method_mismatch`, `expected_method: POST`, both sites, `not_checked`; delta new `POST`, removed `GET` | passes |
| `http-route-rename` | route module renamed to `api.v1.purchases.$orderId.ts` | `missing_endpoint` `GET /api/v1/orders/:encoded` at both sites; delta | passes; `_marking` ignored, carrick#3092 |
| `graphql-field-rename` | `OrderView.total` to `amount` (schema :7, resolvers :3) | `type_mismatch` at `src/order-query.ts:9`, `on_main: false`, `resolved`; marking: `QUERY order` label, `edge_source: fact` | passes; `_marking` ignored, carrick#3090 and carrick#3091 |
| `graphql-field-removal` | `currency` dropped from `OrderView` (schema and resolver type; the answer's resolver line 11 to 10) | as above | as above |
| `graphql-root-rename` | `order` to `orderById` (schema :2, resolvers :11, answer) | `missing_endpoint` `QUERY order` at `src/order-query.ts:9`; delta | passes; `_marking` ignored, carrick#3092 |
| `socket-field-rename` | listener's `OrderUpdate.status` to `state` (`src/realtime.ts:5`) | `type_mismatch` at `storefront` `src/live.ts:12`, `on_main: false`, `resolved`; marking: `CLIENT->SERVER order:update` label, `edge_source` | passes; `_marking` ignored, carrick#3090 and carrick#3091 |
| `socket-field-removal` | the `order:shipped` emit drops `carrier` (`src/realtime.ts:12`) | `type_mismatch` `SERVER->CLIENT order:shipped` at `src/realtime.ts:12` | ignored, carrick#3094, carrick#3090, carrick#3091 |
| `socket-listener-rename` | `order:update` to `order:changed` (`src/realtime.ts:11`) | `missing_endpoint` at `src/live.ts:12`; delta | passes; `_marking` ignored, carrick#3092 |
| `socket-emit-rename` | `order:shipped` to `order:dispatched` (`src/realtime.ts:12`) | `missing_endpoint` at the PR's emit, `orphaned_endpoint`, no delta row | passes; `_marking` ignored, carrick#3092 |
| `pubsub-field-rename` | subscriber's `PaymentSettled.amount` to `total` (`src/events.ts:5`) | `type_mismatch` `PUBSUB payments.settled` at `src/payments.ts:10` | ignored, carrick#3089 |
| `pubsub-field-removal` | publish of `orders.shipped` drops `carrier` (`src/events.ts:15`) | `type_mismatch` `PUBSUB orders.shipped` at `src/events.ts:15` | ignored, carrick#3089 |
| `pubsub-subscriber-rename` | `payments.settled` to `payments.cleared` (`src/events.ts:9`) | `missing_endpoint` at `src/payments.ts:10`; delta | passes; `_marking` ignored, carrick#3092 |
| `pubsub-publish-rename` | `orders.shipped` to `orders.dispatched` (`src/events.ts:15`) | `missing_endpoint` at the PR's publish, `orphaned_endpoint`, no delta row | passes; `_marking` ignored, carrick#3092 |
| `copied-helper` | adds `src/money.ts`, a copy of `storefront`'s `formatMinor` | `functions.changed` has `src/money.ts:formatMinor:added`, no finding | passes |
| `major-bump` | `zod` `^3.22.0` to `^4.0.0` | `dependency_conflict` `zod`, `major`, `changed_by_pr` | passes |
| `dev-only-split` | `typescript` `^5.4.0` to `^6.0.0` under `devDependencies` | no conflict on `typescript` | ignored, carrick#3093 |
| `respell-param` | route param `orderId` to `id` | empty delta, no finding | passes |
| `convention-copy` | route module copied to `api.v1.invoices.$invoiceId.ts` | `loader` added in the new file; delta new route | passes |
| `route-moved-within-its-file` (`admin-api`) | `GET` and `POST` registrations swapped (`src/admin.ts:16-17`) | empty delta | passes |
| `loop-route-edit` (`admin-api`) | edit inside the loop at `src/admin.ts:20` | empty delta | passes |

RPC routers have no seed: no client row exists to pair with (carrick#937).
