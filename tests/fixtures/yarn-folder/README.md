# `yarn-folder`

Fixture for carrick#3683: a repo's committed Yarn release (`.yarn/`) is
Yarn's own code, never the project's. Driven by `tests/yarn_folder_test.rs`.

Answer key: rows come only from `src/`. The `orderPlaced` producer and listener
are the event-bus pass's rows over `src/orders.ts`. Nothing is stated at a
`.yarn/` path: no function, endpoint or call row, although the release file
holds an event emitter, a listener and a function of its own.
