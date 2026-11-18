---
name: carrick-census
description: Use when the ask is which endpoints do X, such as every route that checks an admin role, every endpoint that sends an email or writes to a table, and which apps call them, and before changing a shared helper, to see which endpoints and apps depend on it. Finds the helper by meaning, walks the call graph up to the endpoints, and names each endpoint's callers in other services.
---

# Which endpoints do X, and who calls them

{{SCOPE_NOTE}}

Grep finds the lines that do X. This skill answers what grep cannot: which
endpoints reach those lines, and which apps call those endpoints. Grep for the
raw lines as well; the two answers go side by side.

## 1. Find the shared function that does X

Ask in two wordings, one for what the code is for and one for how it does it:

```
search_by_intent({{SCOPE}}, query: "<what it is for>", also_phrased_as: ["<how it does it>"], compact: true, top_k: 20)
```

Where the answer carries no `phrasings`, run the second wording as its own
search.

Read each row at `file_path` from `line_number` to `end_line`. Keep the rows that
do X, and among them find the lowest: the function the others call to do it,
such as the function that builds the mail client and sends, or the role check
itself. Stop paging when a page holds no row that does X. Do not change
`similarity_threshold` while paging: a new threshold is a new list, so start it
again at offset 0.

Where the question names an identifier (a function, an env var, a table, a
header, a package), grep for it too. For each grep hit inside a function, find
that function with `list_function_intents({{SCOPE}}, file: "<file>", compact: true)`:
the row whose line span holds the hit.

## 2. Walk up to the endpoints

For each shared function from step 1, one call:

```
get_callers({{SCOPE}}, function_name: "<name>", file: "<file>", depth: 3)
```

Each row is a function that holds a call, and its `chain` runs from it down to
the function you asked about. Sort every row into one of these:

- **Route handler.** Confirm it with
  `get_api_endpoints({{SCOPE}}, service: "<service>", path_contains: "<path fragment>")`:
  the endpoint whose `file_location` and `handler_span` hold the row is the one.
  Take the path from that endpoint, never from the handler's name.
- **GraphQL resolvers.** A row named `queryFields_handler` or
  `mutationFields_handler` spans a whole file of resolvers. Read the source at
  `call_site_line` to name the field, then confirm it with
  `get_operation({{SCOPE}}, method: "QUERY" or "MUTATION", path: "<field>")`.
- **Middleware.** A `use_handler` row gates a router. Grep that router's file
  for `.route(` and `.use(` to find what is mounted behind it, list those routes
  with `get_api_endpoints` and `path_contains` on each mount path, and give the
  count.
- **Dead end.** A chain whose top is an anonymous callback (a name like
  `andThen_handler`), a class method, a webhook or event registry, or `<module>`
  (top-level code, whose result is passed around as a value the index does not
  follow). Grep its file for the router or registration call that runs it. If
  that finds the route, add it, marked "found by grep". If not, list it under
  "chains that stop", with file:line and where it stops.

Rows under `unfollowed_calls` may also call the function: read each one in
source and keep it if it does.

## 3. Who calls each endpoint

```
get_operation({{SCOPE}}, method: "<METHOD>", path: "<path>")
```

Read `consumers` only: service and `call_file_location`. Skip `unread_sites`.

## 4. Report

| endpoint | how it does X (call chain) | called by (service, file:line) | found by |
|---|---|---|---|
| GET /bills/:billId/file | handler → isAdminUser | web: src/pages/BillPage.tsx:40 | index |

Then, apart from the table:

- chains that stop, each with file:line and where it stops;
- the lines that do X but reach no endpoint (scripts, top-level code, frontend
  pages), from grep, each with file:line;
- for a question about frontend code only (pages, components), say that the
  table stays empty because the index holds no route list for frontend routers,
  and that the answer is the grep list plus the callers of the shared function;
- the counts: shared functions followed, endpoints reached, callers listed.

Change nothing unless you were asked to.
