# caller-built-body

A caller of an in-repo method whose body the method builds (carrick#4260).
The method sends through the platform `fetch`, which the library store
labels an HTTP client, so the fact layer decides each send at the method.
The request summaries state a row at each caller; the send's site yields to
that row. The row then takes the site's body slot, read through the
caller's own call.

Answer key (request type of the row at each caller, `src/caller.ts`):

| line | caller | request |
|---|---|---|
| 5 | `addWidget` | `{ widget: { label: string; size: number; }; source: "cli"; }` |
| 8 | `placeBook` | `{ item: { title: string; }; }`, never a bare type parameter |
| 9 | `addInline` | `{ name: string; }` |
| 10 | `pingAll` | `no_request_body`: the method sends no body |
| 11 | `rename` | `{ label: string; size: number; }`, from the method's declared parameter |

Read by `tests/caller_built_body_test.rs`. It needs the built compiler
engine. Protocols: HTTP request bodies only.
