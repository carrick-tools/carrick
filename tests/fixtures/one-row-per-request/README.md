# one-row-per-request

One HTTP request, written the way a React client writes it (carrick#1371):

```
useShelves.ts:6   useQuery({ queryFn: () => shelvesApi.listMine() })   setup
useShelves.ts:8     shelvesApi.listMine()                              call through the client
shelves.ts:17       await sendWithAuth(`${SHELF_API_URL}/v1/me/shelves`)  the request
shelves.ts:21       return response.json()                             the body read
```

Every one of those four lines raises an HTTP candidate of its own, and the
analyzer answers each of them with a target. `@example/query` is a neutral
stand-in for a query-hook package; the framework-detect cassette names it as a
data fetcher, which is what raises the candidate at the `useQuery` line.

## The cassette

`__llm__/analyze-file/useShelves.json` answers both lines 6 and 8 with the same
operation, and gives line 6 the `UseQueryResult` anchor — the projection of the
response, which is the type that made a matching contract read as incompatible
(carrick#1375).

`__llm__/analyze-file/shelves.json` answers line 21 only, with
`primary_type_symbol: "ShelfSummary"`. That is the real shape, and the row it
sits on is the one the fold deletes — so the anchor has to move onto line 17,
whose deterministic same-file-wrapper row states a method and a target and no
type at all. Line 17 is not answered by the cassette: the wrapper pass emits
it.

## Answer key

Two call rows:

| Site | Why it survives |
|---|---|
| `src/hooks/useShelves.ts:8` | the call through the client method (carrick#1146) |
| `src/lib/shelves.ts:17` | the request the method's body issues |

Both `GET`, both ending `/v1/me/shelves`. Line 6 folds into line 8 (it encloses
it and states the same operation); line 21 folds into line 17 (it reads the
value line 17 produced) and hands over its type anchor.

Which of the two survivors is the network request and which is the call
through it is not expressible on an indexed row yet; that needs a role field,
which is a change to what the index blob carries.
