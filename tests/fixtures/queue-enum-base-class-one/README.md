# queue-enum-base-class-one

One root service `jobs`: the base-class wrapper of `queue-enum-base-class`
plus four control files (carrick#3527). Same store answer.

Record-level answer key, forced queue name per site:

| Site | Name | When |
|---|---|---|
| `src/c3-param.ts:9` | `alpha-jobs` | today (guard) |
| `src/c1-direct.ts:10` | `beta-jobs` | today (guard) |
| `src/consumer-base.ts:9` | one site, name not forced (opaque `this.topic`) | today (guard) |
| `src/c2-ctor-literal.ts:5` | `ctor-literal-jobs` | constructor bodies read |
| `src/alpha-consumer.ts:11`, `src/gamma-consumer.ts:11` | `alpha-jobs`, `gamma-jobs` | base-class field from `super(...)` |
| `src/send.ts:5`, `:9`, `src/c1-direct.ts:7` | `alpha-jobs`, `gamma-jobs`, `beta-jobs` | maker-bound names |
| `src/c4-literal.ts:6` | exactly one row `literal-jobs` (`library_claim`) | today (guard, in `rows.expected.json`) |
| anywhere | never `job`, never `direct-job` | guard |

Protocol: pub/sub (queues).
