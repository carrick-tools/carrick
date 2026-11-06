# sibling-declared-host

Two services, scanned as separate repos and joined (carrick#3441).

- `ledger-api` declares `internalDomains: ["https://api.ledger.example"]` and serves
  `POST /jobs/run`, `GET /jobs/status`, and `POST /v1/send` (a probe).
- `tool-cli` has no `carrick.json`. It calls `https://api.ledger.example/jobs/run`
  and `/jobs/status`, and `https://api.mailer.example/v1/send`, a host nobody declares.

What the scanner's join does today (pinned by `tests/sibling_declared_host_test.rs`):

| Call | Edge to `ledger-api` | Note |
|---|---|---|
| `POST https://api.ledger.example/jobs/run` | yes | wanted |
| `GET https://api.ledger.example/jobs/status` | yes | wanted |
| `POST https://api.mailer.example/v1/send` | yes | NOT wanted: undeclared host, paired by path (carrick-cloud#656) |

The last row is pinned as found, not fixed here. `__llm__/` holds the mocked model answers.
