# url-to-string-choices

A URL that a conditional chooses, sent through `.toString()`, reads as the
choice (the same value without the call). The test is
`tests/url_to_string_choices_test.rs`.

Answer key (one row per way, no duplicates):

- `src/choices.ts` line 3: a query choice. One row, `GET /api/s1`; no lead.
- line 8: a path choice. `GET /api/s2/one` and `GET /api/s2/two`; no lead.
- line 12: an inline choice. `GET /api/s3/one` and `GET /api/s3/two`.
- line 17: a template with a hole. `GET /api/s4/:id`.
- line 22: a query choice without `.toString()`. `GET /api/s5`.
