# queue-maker-fan-out

One root service `jobs` (carrick#3527): a queue whose name is bound at its
maker (`new Queue(name)` in `open`), sent from `send`, and handed down
through seven levels of two parameter-passing helpers each (`src/relay.ts`).
Every one of the 128 paths from `main` passes `fan-jobs`.

The walk up the maker's callers is capped (`MAX_COMPOSED` calls visited).
This fixture exceeds the cap, so the answer key is the capped reading:

| Site | Name | Why |
|---|---|---|
| `src/open.ts:8` | one site, name not forced (asked), `composed_trimmed` set | the walk stopped at its cap |

Store answer: the same as `queue-enum-base-class-one`.

Protocol: pub/sub (queues).
