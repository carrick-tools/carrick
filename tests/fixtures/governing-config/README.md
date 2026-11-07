Four npm-workspace layouts for carrick#3497, one member `web` each. Every
`web/src/App.*` has `viaAlias()` calling `get` through `@/lib/api` and
`viaRelative()` calling it through `./lib/api` (the control).

- `a-ws`: root `tsconfig.json` is `{}`; `web/tsconfig.app.json` holds `paths`.
- `b-ws`: `web/tsconfig.json` only lists `web/tsconfig.app.json` as a reference.
- `pos-ws`: `paths` in `web/tsconfig.json` (known positive).
- `j-only`: `paths` in `web/jsconfig.json`, JavaScript sources.

Driven by `tests/governing_config_callers_test.rs` and
`tests/service_derivation_test.rs`.
