# wrapper-fixed-verb

A helper whose own request writes `method: "POST"`, and a screen that calls
it. The call names no verb of its own: the verb is the helper's.

```
lib/catalog.ts            syncCatalog(items)  fetch("/catalog/sync", { method: "POST", body })
screens/sync-screen.ts:3  syncCatalog(items)
```

## The cassette

- `sync-screen.json` answers the call with the sync route and no method. An
  unstated method indexes as a GET, which is the defect.
- `catalog.json` answers the helper's own request line with POST.

## The answer key

| Site | Route |
|---|---|
| `lib/catalog.ts:2` | POST /catalog/sync |
| `screens/sync-screen.ts:3` | POST /catalog/sync, or no row of its own |

No row for the route carries any verb but POST.
