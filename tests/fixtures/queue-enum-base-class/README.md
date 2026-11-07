# queue-enum-base-class

A queue named by an enum member passed up a base-class constructor
(carrick#3527). Five services in one repo: `sender` and `runner` use the
wrapper package `queue-kit` (`packages/queue-kit`), `bystander` only builds its
client, `topics` holds the enum (`Alpha`, `Beta` unused, `Gamma`).

`library-claims.json` is the store answer the labels should produce: a bullmq
`Queue.add` whose name is bound at the `Queue` maker, and a bullmq `Worker`
as a `new`-form receive op. The scanner never overrides a label by library name.

Offline, drives `tests/queue_enum_base_class_test.rs`. The answer key
(`rows.expected.json`) holds what passes today. Rows later slices add:

| Site | Expected | Slice |
|---|---|---|
| `services/runner/src/run.ts:4`, `:5` | consumer possible calls | constructor bodies read |
| `services/sender/src/send.ts:4`, `:8` | publisher rows `alpha-jobs`, `gamma-jobs` (never `job`) | maker-bound names |
| the consumer sites | named `alpha-jobs`, `gamma-jobs` | base-class field filled from `super(...)` |

Protocol: pub/sub (queues).
