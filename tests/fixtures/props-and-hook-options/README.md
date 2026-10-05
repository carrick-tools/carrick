# props-and-hook-options

A URL that reaches a request from outside the function that makes it
(carrick#1949): through a hook's options, or through a component's props, and
the request is made inside a callback or a closure. Scanned with the model
off, so every row is one a pass states as a fact.

`a-ui-runtime` and `a-widget-kit` are declared dependencies with no source in
the repository.

## Answer key

| File | Line | Row | Why |
|---|---|---|---|
| BoardPage.tsx | 6 | `POST /resources/boards/:boardId/widgets` | the hook's options carry the URL its callback fetches |
| useBoardEditor.ts | 6 | none | the URL is the caller's |
| BoardPage.tsx | 7 | `PUT /resources/boards/:boardId/sync` | `fetch(syncUrl, init)` in a callback takes the method the caller writes in `init`; `fetch(syncUrl, withCache(init))` states no method, so nothing is guessed |
| AgentPage.tsx | 8 | `POST` and `PUT /resources/agents/:agentId/chat` | the prop carries the URL a closure and a method handed to a package's hook fetch |
| AgentPanel.tsx | 4 | none | the URL is the caller's |
| AgentPage.tsx | 17 | none | a component hands its own prop on |
| AgentPage.tsx | 9 | `POST` and `PUT /resources/agents/main/chat` | its caller writes the prop it hands on |
| AgentPage.tsx | 10 | `DELETE /api/notices/:id` | `props.noticesUrl`, read in a closure |
| AgentPage.tsx | 11 | none | a package's component |

Two closures in `AgentPanel.tsx` add nothing at an element:

- `${endpoint}/refresh` states a route with the prop read by its name, so it
  is stated where it is written, as before (the rule carrick#1950 applies to a
  key of an object parameter).
- `${endpoint}/at${section}${suffix}` glues values inside a segment, which
  states a path the source does not; no row anywhere.

Two readings are decided here. A callback or a closure written in a function
is code that can run, so what it sends through the function's parameters is
the function's to state at its callers. It never makes the function complete,
so no model row is withdrawn because of it. A JSX element is a call of its
component with its attributes as one props object, as the call graph already
reads it (carrick#1149). It states only what its props fill in.
