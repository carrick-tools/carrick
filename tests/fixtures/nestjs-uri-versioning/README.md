# nestjs-uri-versioning

carrick#3422: a decorator-routed service that enables URI versioning serves
each route under the version segment the framework composes, so the index
must state the path with it.

`catalog-api` is the producer. `src/main.ts` enables URI versioning with the
default version `1`. Its controllers:

| file | what it states | served at |
|---|---|---|
| `widgets.controller.ts` | no version; one handler is `VERSION_NEUTRAL` | `/v1/widgets`, `/v1/widgets/:id`, `/widgets/ping` |
| `orders.controller.ts` | object form, `version: "2"` | `/v2/orders` (GET, POST) |
| `reports.controller.ts` | template path; one handler `@Version("3")` | `/v3/reports/daily`, `/v1/reports/weekly` |
| `health.controller.ts` | controller `version: VERSION_NEUTRAL` | `/status/health` |
| `decoy.controller.ts` | class decorator from another module | no row |

`storefront` calls each route through an undeclared environment base (an
expected find, so nothing is declared) and makes two calls nothing serves:
`GET /v1/orders` and `GET /widgets`.

The model is silent for every file (`__llm__/`), so every producer row is the
deterministic layer's. `expected.json` is the answer key.
