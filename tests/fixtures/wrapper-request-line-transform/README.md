# wrapper-request-line-transform

A wrapper's request line that leaves its path to its callers gives its
model row to their rows (carrick#3961). What each caller may carry depends
on what the wrapper hands back:

- the request's own value, unread (a library client's call returned as it
  resolves): the response read at the request line is the caller's;
- the parsed body (`return res.json()`): the caller's own call is typed;
- anything else (an envelope, one field, the raw `fetch` `Response`): the
  call's value is the wrapper's own, so the caller carries no response type
  and gets no response verdict.

Either way the caller keeps the request type the request line sends.

Two services: `api` serves the routes with typed bodies and responses;
`web` calls them through `fetch` wrappers in `web/src/net/` (called from
`web/src/app.ts`) and through same-file wrappers in `web/src/tools.ts`.

| caller | wrapper hands back | request | response |
|---|---|---|---|
| `app.ts:8`, `tools.ts:32` `getJson` (GET) | `{ ok, data }` envelope | none (GET) | none |
| `app.ts:9`, `tools.ts:24` `exchangeToken` | `data.token ?? null` | built body, compatible | none |
| `app.ts:10` `readTicket` (GET) | `res.json()` | none (GET) | compatible |
| `app.ts:11`, `app.ts:16` `streamPost` | the raw `Response` | the request line's payload | none |
| `app.ts:12` `send` | the raw `Response` | its options parameter | none |

The `streamPost` callers' request is the request line's payload
(`body ?? {}`), not the argument the caller passes, so it reads
`incompatible` against `api`. The request line's own row read the same
before carrick#3961; the tests assert the entry, not that verdict.

The model's answer for each request line is in
`model-cassette/analyze-file/<file stem>.json`.

Read by `tests/wrapper_request_line_test.rs`. It needs the built sidecar.

Protocols: HTTP only.
