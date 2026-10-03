# wrapper-call-route

A helper module that declares one function per sibling route, and two hooks
that call them (carrick#1794):

```
shelves.ts:13      fetchShelfStats(shelfId?)        path chosen by a ternary, GET /v1/shelves/stats
shelves.ts:22        request({ path: `/v1/shelves/shared-stats?...`, method: "GET" })
useShelfStats.ts:4        fetchShelfStats(shelfId)
useSharedShelfStats.ts:4  fetchSharedShelfStats(shelfId)
```

`transport.ts` hands the options to a client package with a spread, so no
deterministic pass reads the requests: every row is the model's.

## The cassette

The answers are the shape the model gives for this code:

- `shelves.json` answers line 22 with the shared route, and gives the
  ternary helper no row.
- `useShelfStats.json` answers the call of `fetchShelfStats` with the stats
  route, which is what that helper requests.
- `useSharedShelfStats.json` answers the call of `fetchSharedShelfStats` with
  the **stats** route too. That is the defect: the helper it calls writes and
  requests the shared route.

## The answer key

| Site | Route |
|---|---|
| `useShelfStats.ts:4` | GET /v1/shelves/stats |
| `shelves.ts:22` | GET /v1/shelves/shared-stats |

The shared-stats hook's row takes its helper's route. The helper's own request
line already states that route with a candidate behind it, so the graph keeps
that one row for the one request (the wrapper-echo rule), and no row names the
stats route at the shared-stats hook.
