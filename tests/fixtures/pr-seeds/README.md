# pr-seeds: one seeded PR change per finding kind and per protocol

Four repos and the contracts between them, used by the `seed_*` tests of
`tests/pr_on_main_test.rs` (carrick#3095). Each seed edits one repo on a branch
and runs a real PR scan against main's stored index. The tests replay the
model from hand-written answers in each repo's `__llm__/`, and run with the
five fact-layer switches the cloud runner sets.

`seeds.json` is the one definition of each seed: its edits, which the tests
apply with `apply_seed`, and the check a correct Carrick posts for it
(`expect`: title, conclusion and every row, a closed world). The PR-accuracy
check that runs before each release replays the same seeds with the candidate
build and scores the rendered check against `expect`. `change` is the kind of
change the seed makes. `known_miss` is `{stage, issues}`: the first stage
(found, paired, judged, finding, shown) at which the check for the seed is
wrong today, and the issues it waits on. Every seed test holds the payload to
that stage (`tests/support/seed_stages.rs`), in both directions, so closing a
miss moves the declaration in the same PR. A seed is removed only by
`retired: {date, reason}`; `scripts/seed-retirement-check.py` fails a deletion.

- `orders-api` serves an HTTP route (`app/routes/api.v1.orders.$orderId.ts`),
  a GraphQL schema and resolver (`src/schema.graphql`, `src/resolvers.ts`), a
  socket listener and one emit (`src/realtime.ts`), and a pub/sub subscriber
  and publisher (`src/events.ts`). For the type-reach seeds (carrick#3102) it
  also holds a contracts barrel (`src/contracts/`): the route's `Order` and
  the listener's `OrderUpdate` each intersect one of its two types, imported
  on line 2 so no other line moves. `src/augment.ts` augments
  an interface of the contracts module, and `src/legacy.js` is a CommonJS helper outside the
  tsconfig's program. Each has an empty answer.
- `storefront` is the other side of each. Seeds never edit it, so every break
  is a consumer the PR did not update.
- `admin-api` registers routes in code through a router stub, for two
  negatives. It scans against a store of its own: its stub pins a package no
  registry has, which would make every cross-repo type check in a shared store
  unverifiable.
- `rpc-api` serves a procedure router (a tRPC-shaped `t.router` from the
  `fixture-procs` stub) under a fetch adapter at `/api/trpc`, and a web sub-app
  mounted under `/api/v1`; `src/web/orders-view.ts` calls one procedure through
  a typed client. Its library answers are `procedure-router`'s. It has a store
  of its own for the same reason as `admin-api`.
- `rpc-shelf` (carrick#3778) is two services in one repo, with a store of its
  own: `api` serves a `shelf` procedure router (`shelf.list`, `shelf.get`,
  `shelf.add`) under a fetch adapter at `/api/rpc`, and `web` calls it through
  a typed client whose router type is imported from `api`. One call
  (`shelf.gone`) is to a procedure nothing serves; it is no row. Its library
  answers are the server labels of `rpc-api` plus the client's.

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
| `socket-field-removal` | the `order:shipped` emit drops `carrier` (`src/realtime.ts:12`) | `type_mismatch` `SERVER->CLIENT order:shipped` at `src/realtime.ts:12` | passes; shown waits on carrick-cloud#2829 |
| `socket-listener-rename` | `order:update` to `order:changed` (`src/realtime.ts:11`) | `missing_endpoint` at `src/live.ts:12`; delta | passes; `_marking` ignored, carrick#3092 |
| `socket-emit-rename` | `order:shipped` to `order:dispatched` (`src/realtime.ts:12`) | `missing_endpoint` at the PR's emit, `orphaned_endpoint`, no delta row | passes; `_marking` ignored, carrick#3092 |
| `pubsub-field-rename` | subscriber's `PaymentSettled.amount` to `total` (`src/events.ts:5`) | `type_mismatch` `PUBSUB payments.settled` at `src/payments.ts:10` | passes; shown waits on carrick-cloud#2829 |
| `pubsub-field-removal` | publish of `orders.shipped` drops `carrier` (`src/events.ts:15`) | `type_mismatch` `PUBSUB orders.shipped` at `src/events.ts:15` | passes; shown waits on carrick-cloud#2829 |
| `pubsub-subscriber-rename` | `payments.settled` to `payments.cleared` (`src/events.ts:9`) | `missing_endpoint` at `src/payments.ts:10`; delta | passes; `_marking` ignored, carrick#3092 |
| `pubsub-publish-rename` | `orders.shipped` to `orders.dispatched` (`src/events.ts:15`) | `missing_endpoint` at the PR's publish, `orphaned_endpoint`, no delta row | passes; `_marking` ignored, carrick#3092 |
| `copied-helper` | adds `src/money.ts`, a copy of `storefront`'s `formatMinor` | `functions.changed` has `src/money.ts:formatMinor:added`, no finding | passes |
| `major-bump` | `zod` `^3.22.0` to `^4.0.0` | `dependency_conflict` `zod`, `major`, `changed_by_pr` | passes |
| `dev-only-split` | `typescript` `^5.4.0` to `^6.0.0` under `devDependencies` | no conflict on `typescript` | ignored, carrick#3093 |
| `respell-param` | route param `orderId` to `id` | empty delta, no finding | passes |
| `convention-copy` | route module copied to `api.v1.invoices.$invoiceId.ts` | `loader` added in the new file; delta new route | passes |
| `route-moved-within-its-file` (`admin-api`) | `GET` and `POST` registrations swapped (`src/admin.ts:16-17`) | empty delta | passes |
| `loop-route-edit` (`admin-api`) | edit inside the loop at `src/admin.ts:20` | empty delta | passes |
| `route-added-in-unparseable-file` (`admin-api`) | a route added below the loop in `src/admin.ts`, its handler left with a rebase conflict, so the file does not parse; no line above moves, so the answer is not edited (an answer edit is an input change that moves the service's reading) | empty delta; `delta.files_not_read` names `src/admin.ts`, `does_not_parse`, with main's routes there | passes; shown waits on carrick-cloud#3207 |
| `http-field-type-change` | route field `total` from `number` to `string` | the same two rows as `http-field-rename` | passes |
| `http-route-removed` | route module deleted | `missing_endpoint` at both sites; delta removed | passes |
| `http-handler-body` | the loader's body changes, its type does not | empty delta, no introduced risk | passes |
| `non-code-edit` | adds `docs/orders.md` | empty delta, no finding | passes |
| `http-prefix-moved` (`rpc-api`) | sub-app mount `/api/v1` to `/api/v2` | delta new `/api/v2/reports/daily`, removed `/api/v1/...` | passes |
| `http-edit-under-prefix` (`rpc-api`) | a handler body under the mounted prefix | empty delta | passes |
| `http-path-into-option-default` (`rpc-api`) | `/reports/daily` moves into a destructured option whose default is the same path (`src/server.ts:7`) | empty delta | passes |
| `trpc-procedure-added`, `-removed`, `-renamed` (`rpc-api`) | `orders.cancel` added, `orders.create` removed, `create` to `place` | delta under the adapter's endpoint (`POST /api/trpc/orders.cancel`) | passes |
| `trpc-resolver-body` (`rpc-api`) | a mutation's body | empty delta, no introduced risk | passes |
| `trpc-adapter-prefix-moved` (`rpc-api`) | the adapter's endpoint `/api/trpc` to `/trpc` | every procedure new under `/trpc`, removed under `/api/trpc` | passes |
| `trpc-output-field-removed` (`rpc-api`) | `orders.list` output loses `total`, which the client reads | `type_mismatch` at `src/web/orders-view.ts:7` | ignored, carrick#937 (no procedure client call is read) |
| `trpc-called-procedure-removed` (`rpc-shelf`) | `shelf.get`, which `web` calls, is deleted from the router | delta removed `GET /api/rpc/shelf.get`; the correct check also carries `removed_still_called` and fails | stage test passes; `_marking` ignored, carrick#3778 |
| `trpc-called-procedure-renamed` (`rpc-shelf`) | `get` to `fetch` in the router only | delta new `.../shelf.fetch`, removed `.../shelf.get`; correct check fails | as above |
| `trpc-procedure-added-beside-calls` (`rpc-shelf`) | `shelf.put` mutation added | delta new `POST /api/rpc/shelf.put`, no risk | passes |
| `trpc-adapter-mount-moved-with-client` (`rpc-shelf`) | adapter `/api/rpc` to `/rpc`, client `url` with it | three new under `/rpc`, three removed under `/api/rpc`, no risk | passes |
| `trpc-adapter-mount-moved-client-stale` (`rpc-shelf`) | adapter moves, client `url` stays | the delta rows; the correct check fails (the client now misses the mount) | stage test passes; `_marking` ignored, carrick#3778 |
| `trpc-router-binding-renamed` (`rpc-shelf`) | `appRouter` to `shopRouter`, alias and client import with it | empty delta, no risk | passes |
| `trpc-router-rewritten-unread` (`rpc-shelf`) | the `shelf` keys written as computed keys the reader cannot read | the correct check sees no change; main posts three removed rows | stage test passes; `_marking` ignored, carrick#3778 |
| `graphql-field-added` | `OrderView` gains nullable `note` (schema, resolver type, the answer's resolver line) | no introduced risk, no call left without a producer, empty delta | passes (carrick#3172) |
| `graphql-root-added` | a root query `orders` no client calls | as above; delta new `QUERY orders` | passes (carrick#3172) |
| `graphql-resolver-body` | the `order` resolver's body | as above, empty delta | passes (carrick#3172) |
| `socket-listener-field-added` | the listener's `OrderUpdate` gains optional `note` | as above, empty delta | passes (carrick#3172) |
| `socket-event-added` | a new `order:note` listener that relays to every client | as above for the old events; the relay's emit is one gap (`missing_endpoint`), as in `socket-emit-rename`; delta new listener | passes (carrick#3172) |
| `pubsub-subscriber-field-added` | the subscriber's `PaymentSettled` gains optional `currency` (the answer's lines move) | as above | passes (carrick#3172) |

## Type-reach seeds (carrick#3102)

`reach_*` tests of `tests/pr_on_main_test.rs`. Each edits `orders-api` and
reads its dumped decision (`CARRICK_TYPE_REACH_DUMP`).

| Seed | Edit | Asserts |
| :--- | :--- | :--- |
| `a_contract_edit_reaches_only_its_routes_aliases` | `src/contracts/order-ref.ts` gains a field | partial: only HTTP aliases reached |
| `a_resolver_edit_reaches_only_graphql_aliases` | `src/resolvers.ts:7` initial value | partial: only GraphQL aliases reached |
| `an_edit_to_the_socket_file_reaches_only_socket_aliases` | `src/realtime.ts:8` port | partial: only socket aliases reached |
| `an_edit_to_the_publisher_reaches_only_pubsub_aliases` | `src/events.ts:10` | partial: only pub/sub aliases reached |
| `an_augmentation_edit_is_whole` | `src/augment.ts` | `whole (augmentation)` |
| `a_schema_edit_is_whole` | `src/schema.graphql` | `whole (repo_input)` |
| `a_tsconfig_edit_is_whole` | `tsconfig.json` target | `whole (config_file)` |
| `off_is_whole_and_changes_no_payload` | the contract edit, with `CARRICK_TYPE_REACH=off` | `whole (off)`, and the payload equals the run with the decision on |
