# own-route-calls

One service whose pages, components and helper module call the API routes the
same service defines (carrick#1926, carrick#1944). Every name in it is
invented.

A call to a route of the calling service is a consumer of that route. The scan
keeps it as a call row marked `own_route`, and the pair is an edge whose
producer and consumer are the one service. Until that was ruled, the scan
deleted every such row, so a one-service app indexed no consumer of its own
API: of this fixture's 36 rows it kept 1.

## Shape

Six route files under `app/api/` export 11 method handlers. 25 `fetch` sites
call them, and 4 more sites call the helper module that does.

| File | `fetch` sites | Rows |
|---|---|---|
| `components/ItemList.tsx`, `ItemDetail.tsx`, `ReportTable.tsx`, `ArchiveButton.tsx`, `ItemForm.tsx` (client components) | 13 | 16 |
| `app/items/new/page.tsx` (client page) | 1 | 1 |
| `app/dashboard/page.tsx` (server component) | 6 | 8 |
| `lib/items-api.ts` (helper module) | 5 | 7 |
| `components/ItemPicker.tsx`, `ItemPickerAlias.tsx` (call the helper, by relative import and by `@/` alias) | 0 | 4 |
| `app/layout.tsx`, `components/Badge.tsx` | 0 | 0 |

A site has two rows where it sends one of two requests: a method held in a
variable (`archived ? 'DELETE' : 'POST'`), or a ternary that picks the URL and
the method together.

The call shapes, each in its own function: a literal path, a path parameter, a
query string in five spellings (after `?` in the literal, `params.toString()`,
a path held in a binding, a concatenation, a query held in a binding), a
method in a variable, and a ternary picking URL and method.

## Answer key

`expected.json` lists every row, 36 of them. 35 are marked `own_route`. The
one that is not is `app/dashboard/page.tsx:22`, written against
`process.env.APP_URL`: the scan cannot say where an undeclared base goes, so
it matches no route and is not marked.

`__llm__/` holds the mocked model answers for a `CARRICK_MOCK_ALL` scan. They
state every call with the method and path it sends, so the test measures what
the scanner does with a correct reading. The 4 rows at the helper's callers
come from no answer: the scanner states them itself.

A marked call has no type entries yet. Same-service pairs join the type check
in carrick#1945, and the entries come with them.

Used by `tests/own_route_calls_test.rs`.
