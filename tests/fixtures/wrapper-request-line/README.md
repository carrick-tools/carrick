# wrapper-request-line

A wrapper whose request line leaves the path to its callers
(`http.post(`/v1${endpoint}`)`) is one request per caller, stated in each
caller's row. The model's row at the wrapper's own request line restates
them, so it goes, whatever path it reads (carrick#3961). It stays when a
caller is not read (an unread site), or when a function holding the
request open has no caller the scan sees. A call made where a module loads
states the request in a fact row of its own, so the request line's row goes
there too (carrick#4121).

One wrapper per file in `repo/src/net/`, each with the model's answer for
its request line in `model-cassette/analyze-file/<file stem>.json`:

| wrapper | callers | expected at the request line |
|---|---|---|
| `orders` | `checkout.ts` x2, one in its own file | no row |
| `ping` | `health.ts` | no row |
| `billing` | `invoice.ts`; the model reads a host the code does not write | no row |
| `reports` | `daily.ts`, one with an argument not read | the model's row |
| `relay` | none | the model's row |
| `forward` | `send.ts`; `forwarding.ts` holds it open, uncalled | the model's row |
| `notify` | `alerts.ts`, one call at module level | no row |

`rows.expected.json` is the answer key, read by
`tests/wrapper_request_line_test.rs`.

Protocols: HTTP only. A socket emit or topic publish through a wrapper is
read by another pass: not covered.
