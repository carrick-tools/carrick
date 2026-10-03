# wrapper-call-value

Two hooks that call a helper in another module (carrick#1801):

```
shelves.ts:13       fetchShelfStats(shelfId?)    path chosen by a ternary, GET /v1/shelves/stats,
                                                 body parsed into a copy by parseShelfStats
shelf.ts:8          fetchShelf(shelfId, init?)   GET /v1/shelves/${id}, returns (await res.json()) as Shelf
useShelfStats.ts:4  fetchShelfStats(shelfId)
useShelf.ts:4       fetchShelf(shelfId, init)
```

`transport.ts` hands the options to a client package with a spread, so no
deterministic pass reads the stats request. `fetchShelf` takes its request
options from its caller, which passes its own parameter on, so its verb is
not stated anywhere and the request summaries state no row at the hook; they
still read that `fetchShelf` makes one request and hands back its parsed body.
Both hooks' rows are the model's.

## The cassette

The answers are the shape the model gives for this code:

- `shelves.json` gives `fetchShelfStats` no row: its path is picked by a
  ternary.
- `useShelfStats.json` answers the call of `fetchShelfStats` with the stats
  route, and names `ShelfStats` as the type the call returns.
- `useShelf.json` answers the call of `fetchShelf` with the shelf route, and
  names `Shelf`.

## The answer key

| Site | Route | Consumer response type |
|---|---|---|
| `useShelfStats.ts:4` | GET /v1/shelves/stats | none |
| `useShelf.ts:4` | GET /v1/shelves/:shelfId | stated |

The stats hook's value is what `fetchShelfStats` returns: a copy the helper
builds from the body, not the body. The row keeps its route and states no
consumer response type; its request entry stays. The shelf hook's value is the
parsed body, so its row keeps both entries.
