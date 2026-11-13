# unanswered-on-row-line

`sendUpdate(input, init)` in `src/net/transport.ts` hands its options to
`fetch` through `asUpdate`, which may write `method: 'PUT'` over the
caller's. The code states the path at `src/settings.ts:4` but not the
method, so the scan asks the cloud for the method. Offline nobody answers.
The model's row (`POST /api/settings`) sits on the same line. That row is
the model's answer, not something the code states, so the site is still
unread (carrick#4123). The test is `tests/unanswered_on_row_line_test.rs`.

Answer key:

- `src/settings.ts:4`: the model's row `POST /api/settings` stays. The site
  is saved as an unread site, reason `unanswered`, slot `method`. It is no
  possible call.
- `src/settings.ts:9` (control): `GET /api/ping`, stated by the request
  summaries. Nothing was asked, and no unread site is saved.
