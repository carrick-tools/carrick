# conditional-request

Requests whose URL (carrick#2050) or method (carrick#2051) a conditional
chooses. Each branch of the URL conditionals below writes two segments, so a
single placeholder for the conditional's value is a path one segment shorter
than either route.

## `src/content.ts` (carrick#2050)

| Line | Shape | Rows |
|---|---|---|
| 5 | segment chosen in a binding, URL a template at the call | `POST /api/content/drafts/${id}/publish`, `POST /api/content/posts/${id}/publish` |
| 11 | the same, the URL held in a binding | the same two |
| 15 | the conditional is the URL argument | the same two |
| 21 | one branch is a call's value, URL held in a binding | none |
| 26 | the same, URL a template at the call | the model's row, kept as the model stated it |
| 32 | the conditional leads the URL (a base) | `GET ${base}/health`, as before |
| 38 | two parameters, neither read | `GET /api/content/${owner}/items`, as before |
| 44 | the conditional adds a query | `GET /api/content` |
| 48 | `isDraft` and `!isDraft` in one template at the call | `POST /api/content/drafts/${id}/save`, `POST /api/content/posts/${id}/publish` |
| 53 | `kind === "draft"` and `kind !== "draft"`, URL in a binding | the same two |
| 58 | `kind === "a"` and `kind === "b"`, URL in a binding | none |
| 63 | `isDraft` and `archived`, URL in a binding | all four combinations |
| 68 | an else-if chain on `mode`, URL in a binding | one row per branch, three |
| 73 | that chain in one segment, `mode === "copy"` alone in another | none |

The model's cassette (`__llm__/analyze-file/content.json`) states a row at
lines 5 and 26, and both routes at line 15.

## `src/notes.ts` (carrick#2051)

The model's cassette states the row the field showed at every site: one
method with the URL of the other branch.

| Line | Shape | Rows |
|---|---|---|
| 6 | method and URL in locals, one test | `PATCH /api/notes/${existing.id}`, `POST /api/notes` |
| 10 | the same test written in the call | the same two |
| 17 | method chosen, URL fixed | `POST` and `PUT /api/notes/reorder` |
| 23 | two different tests | the model's row, no structural row |
| 29 | a test that calls a function | the model's row |
| 36 | a test whose name the function assigns again | the model's row |
| 41 | the method is a parameter | none |
| 47 | `!existing` beside `existing` | the same two as line 6 |
| 53 | `mode === "edit"` beside `mode === "copy"` | none |
| 59 | a shared test, and `mode === "replace"` beside `mode === "append"` | none |
| 65 | one else-if chain on `mode` for both | `PATCH /api/notes/${id}`, `PUT /api/notes/${id}/copy`, `POST /api/notes` |
| 71 | the chain for the URL, `mode === "copy"` alone for the method | none |

The cassettes' candidate ids are byte offsets into the sources; editing a
source moves them, so re-read them from a `CARRICK_EVAL_DUMP_DIR` run.
