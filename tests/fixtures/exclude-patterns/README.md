# exclude-patterns

A repository that leaves folders out of its scan with `exclude` in
carrick.json (carrick#1990). The patterns are gitignore syntax, relative to
the service's directory:

```json
{ "exclude": ["scripts/", "app/api/legacy/", "**/*.scratch.ts"] }
```

## Answer key

| Path | Excluded by | In the index |
|---|---|---|
| `app/api/orders/route.ts` | nothing | the route `GET /api/orders`, and its function |
| `lib/client.ts` | nothing | the call `GET /api/orders`, and `loadOrders` |
| `lib/orders.ts` | nothing | `firstOrder`, typed through `OrderRow`, which it imports from an excluded file |
| `app/api/legacy/route.ts` | `app/api/legacy/` | nothing: no route, no function |
| `scripts/backfill.ts` | `scripts/` | nothing: no call, no function |
| `scripts/rows.ts` | `scripts/` | nothing, but the compiler still reads it for `lib/orders.ts` |
| `lib/try.scratch.ts` | `**/*.scratch.ts` | nothing: no call, no function |

The scan prints `Left out 4 file(s) matching the 3 exclude pattern(s) in
carrick.json.`, and the blob's `config_json` carries the three patterns.
