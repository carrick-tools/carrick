# site-wrapper-response-sibling (carrick#4755 T0)

Two services: `services/app` and `packages/net` (a sibling, imported as
`@fixture/net`). One model cassette per file, one row at the first labelled
call. Answer key, read by
`a_wrapper_over_a_sibling_helper_reads_its_response_at_the_deciding_call`
(`tests/site_payloads_test.rs`):

| Site | Shape | Response |
|---|---|---|
| `services/app/src/load.ts:8` | own wrapper `loadGone` over a generic helper `clearAs<T>` in the sibling | `{ gone: boolean; }` |
| `services/app/src/load.ts:9` | own wrapper over a sibling helper that states its own type | `{ wiped: number; }` |
| `services/app/src/load-own.ts:8`, `:9` | the same code with the helpers in the app's own files (control) | `{ gone: boolean; }`, `{ wiped: number; }` |
| `services/app/src/orders-own.ts:2` | a maker in the app's own files | `{ ok: boolean; }` |

`orders.ts:2` and `gate-for.ts:3` (a sibling maker) stay unread: a known gap,
not asserted. Generated from `tests/fixtures` split-fixture shapes; the
`node_modules` copies are committed with `git add -f`.
