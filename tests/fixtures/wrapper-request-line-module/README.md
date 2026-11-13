# wrapper-request-line-module

A wrapper whose request line leaves the path to its callers
(`http.post(`/v1${endpoint}`)`), called where a module loads
(`export const warm = sendWarm('/warm')`). The fact layer states each such
call as a row of its own. Once every call that holds the request line open
is one whose fact row stands, the model's row at the request line is the
same request stated again, so it goes (carrick#4121). That holds for every
call the summaries state no row for, wherever it is written: a constructor
body, a callback, an optional call (carrick#4127). Any call the scan cannot
settle that way keeps the request line's model row.

One wrapper per file in `repo/src/net/`, each with the model's answer for
its request line in `model-cassette/analyze-file/<file stem>.json`. The
calls are in `repo/src/load.ts` and `repo/src/more.ts`:

| wrapper | calls | expected at the request line |
|---|---|---|
| `sendWarm` | one, at module level | no row |
| `sendArg` | one, at module level, with an argument the fact layer cannot decide | the model's row |
| `sendBoot` | one in a constructor, one at module level | no row |
| `sendTimer` | one in a module-level callback, one at module level | no row |
| `sendOpt` | one optional call (`sendOpt?.()`) | no row |
| `sendNested`, through `viaHelper` | one, in a callback through a helper | no row |
| `Client` | the request is written in its constructor (carrick#2404) | the model's row |
| `sendLeft`, `sendRight` | one each, both on one line | the model's row |

`rows.expected.json` is the answer key, read by
`tests/wrapper_request_line_module_test.rs`.

Protocols: HTTP only. A socket emit or topic publish through a wrapper is
read by another pass: not covered.
