Layouts for carrick#3497, the one rule for which tsconfig governs a file
(`docs/reference/module-resolution.md`). Every `App.*` has a `viaAlias*()`
calling `get` through an alias and a `viaRelative*()` calling it through
`./lib/api` (the control).

- `a-ws`: npm workspace; root `tsconfig.json` is `{}`; `web/tsconfig.app.json` holds `paths`.
- `a-cj`: `a-ws` with a `carrick.json` naming `web` and no `tsconfig`.
- `a-solo`: `a-ws/web` scanned alone (only `tsconfig.app.json`).
- `b-ws`: `web/tsconfig.json` only lists `web/tsconfig.app.json` as a reference.
- `b-solo`: `b-ws/web` scanned alone.
- `pos-ws`: `paths` in `web/tsconfig.json` (known positive).
- `j-only`: `paths` in `web/jsconfig.json`, JavaScript sources.
- `j-under-cj`: `j-only` under a root `tsconfig.json` `{}`, with a `carrick.json`
  naming `web` and no `tsconfig`.
- `refs-named`: `carrick.json` names the root `tsconfig.json`, which lists
  nothing and references `apps/one` and `apps/two`, each declaring its own
  `paths` (`@one/*`, `@two/*`); carrick#1425.

Driven by `tests/governing_config_callers_test.rs` and
`tests/service_derivation_test.rs`.
