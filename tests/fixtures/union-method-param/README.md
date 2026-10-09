# union-method-param

Requests whose method is a function parameter (carrick#2049). The model's
answer for each request in `src/members.ts` states the target and no method,
which the scanner indexes as a GET (the cassette in `__llm__/analyze-file/`).

| Line | Shape | Rows |
|---|---|---|
| 4 | `method: "POST" \| "DELETE"`, URL a template written at the call | DELETE, POST |
| 11 | `method: Verb`, an alias the file declares | DELETE, POST |
| 16 | the same, URL held in a binding | DELETE, POST |
| 20 | `method: string` | GET (the model's) |
| 24 | `method?: Verb`, optional | GET (the model's) |
| 29 | `method: Verb`, assigned again in the body | GET (the model's) |
| 33 | `method: Verb`, URL leads with an environment base | GET (the model's) |
| 37 | `method: "POST" \| "DELETE"`, called once with "DELETE" | DELETE |
| 41 | `method: Verb`, called with "POST" and with a parameter of the caller's | DELETE, POST |
| 46 | `method: "POST" \| "DELETE"` on a function written inside another | the model's POST: its callers are not resolved |

Lines 4 to 16 have no caller in the repo, so the declaration is all there is
to read. Where every call writes a verb (line 37) the request line states
those, and where one does not (line 41) it states every verb the declaration
allows. `src/caller.ts` states a caller's own verb as a caller always did:
line 4 DELETE, line 8 POST, line 9 nothing.

The cassette's candidate ids are byte offsets into `src/members.ts`. Editing
the source moves them; re-read them from a `CARRICK_EVAL_DUMP_DIR` run.
