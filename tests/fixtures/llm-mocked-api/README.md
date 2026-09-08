# `llm-mocked-api`

The cassette hard gate's fixture: a small service replayed through the whole
scanner binary from frozen LLM responses in `__llm__/`, asserted against
`__golden__.json`. See `tests/llm_cassette_hard_gate_test.rs` for what the gate
claims and how to re-record.

## Why it carries a `carrick.json`

`src/client.ts` calls `https://orders.internal/api/orders` — a sibling service
in the same system, reached at a real hostname, which is what the fixture has
always meant it to be.

Since carrick-cloud#656 an origin only leaves a call's match key when something
says it is not a third party: a declared-internal base, a plain relative path,
or a loopback literal. `internalDomains` is what says it here. Remove the
declaration and the call keys verbatim and carries no contract type, exactly as
any undeclared literal host does — and the golden's call-side type assertions
go with it.

So the declaration is part of the fixture's shape, not a workaround for the
gate.
