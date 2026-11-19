# reexport-emit: answer key

Test: `tests/reexport_emit_test.rs`. Four services call `this.hub.fire(...)`
at `src/orders.ts:7` on a class the package `hub-kit` exports. Each arm is a
store answer replayed offline (`library-claims-*.json`).

Packages (inside each service's `node_modules`):

- `hub-kit` declares `Local` and re-exports `Hub` from its dependency
  (`export { Hub } from 'hub-core'`).
- `hub-core` declares `Hub`.

Arms:

- `kit`: labels on `hub-kit` (`Hub`, `Local`), `of: "type"`.
- `kit-instance`: as `kit`, with `of: "instance:new"` and a `new` maker. The
  shape the live store labels an event emitter in.
- `kit-listed`: `kit` plus `type_packages: ["hub-core"]` on `hub-kit`.
- `core`: the labels on `hub-core`.

Services:

- `control` declares `Local` in `hub-kit`.
- `asked` imports `Hub` from `hub-kit`, by value in `module.ts` (`forRoot`), so
  the package is asked and answered.
- `declared` lists `hub-core` in its own `package.json`.
- `reexport` imports `Hub` from `hub-kit` as a type only: the scan asks the
  store nothing. No assert in this test; kept for the follow-up that asks it.

Expected, per service and arm (lead = a possible call at line 7 with the
labelled end shown, plus an unread site `unanswered` slot `kind` there):

| service | kit | kit-listed | core | kit-instance |
|---|---|---|---|---|
| control | lead `hub-kit/Local/fire` | same | none asserted | same |
| asked | lead `hub-kit/Hub/fire` | same | none asserted | same |
| declared | none asserted | none asserted | lead `hub-core/Hub/fire` | none asserted |
| reexport | none asserted | none asserted | none asserted | none asserted |

`asked` in `module.ts:4` (`forRoot`) stays an unread site `package_unmatched`
in `kit` and `kit-instance`: no claim names that member.
