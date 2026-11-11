# version-segments

A client reads a path segment (or a topic, an event, the method) from a
destructured options key with a default (carrick#3608):
`const { version = 'v1' } = options || {}`, or `{ version = 'v1' }: O = {}`.
Each caller sends the key its object writes, else the default, also through
a wrapper that spreads its caller's options after its own keys
(`{ version: 'v2', ...options }`: the caller's key, else `v2`).

One client per shape in `repo/src/client/`, one caller file per shape in
`repo/src/use-*.ts`, so no same-file wrapper row masks the result.

Two shapes are pinned at today's reading, for later tickets to move:
`use-whole.ts` (the key is a whole path segment, `/${version}/items`) keeps
the callee's `/:version/items` placeholder (carrick#3608 T2), and a direct
call that fills two name parameters at once with no options bag the summary
reads is asked rather than stated (carrick#3608 T3), so most HTTP clients
here send an options bag (`{ body }`), which the summaries read.
`use-based.ts` (HTTP) and `use-event.ts` (socket) are read by the caller
walk alone.

`rows.expected.json` is the answer key, read by
`tests/version_segments_test.rs`: `rows` maps a call line to the one row the
scan writes there, not found by the model (`METHOD path`, or the event
name); a `kind_asked` line is a broker call, whose label leaves the kind to
the model, so it stays a possible call.

Protocols: HTTP calls, pub/sub topics (kind asked), socket events. GraphQL
names come from documents, not from these keys: not covered.
