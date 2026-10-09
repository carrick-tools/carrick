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

The model's cassette (`__llm__/analyze-file/content.json`) states a row at
lines 5 and 26, and both routes at line 15. Its candidate ids are byte offsets into
`src/content.ts`; editing the source moves them, so re-read them from a
`CARRICK_EVAL_DUMP_DIR` run.
