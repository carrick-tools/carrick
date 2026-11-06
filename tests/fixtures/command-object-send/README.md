# command-object-send

Sends written as command objects (carrick#3439 T1). Every name is made up.
`repo/` is one root service (`orders-api`); `library-claims.json` is the
store answer replayed for it: `SendMessageCommand` (queue sends) and
`PublishCommand` (topic sends) are `broker` exports whose `send` op is
written in the `new` form on the export itself (`form: new`, member null),
the name at `QueueUrl` / `TopicArn` and the payload at `MessageBody` /
`Message` of the constructor's one argument.

`rows.expected.json` is the key of the scan's pub/sub output:

- `publishers`: the two literal names, each a `library_claim` fact at the
  line of the `new` (`queues.ts:8`, `topics.ts:8`).
- `possible_calls`: the three sends whose name is not a literal, each at
  the line where the name is decided and none dropped: `queueAudit` reads
  an environment variable (`queues.ts:17`), and `sendTo(url, body)` takes
  its name from a parameter that each of its two callers fills with an
  environment read (`queues.ts:29`, `queues.ts:33`). The replay has no
  cloud answer, so each reads `unanswered`; with an answer the site rules
  of `src/site_rows.rs` decide, and the site is a row or a possible call,
  never dropped.

The environment variables are not declared in the fixture: an undeclared
read is an expected find.

Not covered, named as gaps: `PutEventsCommand` and the batch send name their
topic inside an array (`Entries[].DetailType`), which the store's slot shape
cannot say yet; direct invokes and table access are other slices.
