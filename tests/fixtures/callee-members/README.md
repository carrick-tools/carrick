# Library members held as values (carrick#3504 S4a, carrick#3485 row 4)

`repo/services/web` has one service. `@fixture/http` labels `http.post` as a
request op and `@fixture/queue` labels `bus.publish` as a send op
(`library-claims.json`). Each `m-*.ts` file holds the member in a value and
calls the value. Discovery's test `a_labelled_members_value_reaches_its_op`
reads the call at the line below.

A member read off a labelled export and held as a value (bound or not)
ends at the op the member's declaration names, at the call that runs the
value; the op's name is read from that call's arguments.

## Answer key

| File | Line | Value | Recorded |
|---|---|---|---|
| `m-bind.ts` | 6 | `const post = http.post.bind(http)` | site, `http` |
| `m-extract.ts` | 6 | `const post = http.post` | site, `http` |
| `m-bind-inline.ts` | 4 | `http.post.bind(http)(…)` | site, `http` |
| `m-publish-bind.ts` | 6 | `const pub = bus.publish.bind(bus)` | site, `bus` |
| `m-fallback.ts` | 9 | `deps.post ?? http.post.bind(http)` | `several_values` (the own entry `deps.post` is not read yet: carrick#3504 S4b) |
| `m-own-bind.ts` | 10 | `send.bind(null)`, `send` calls `http.post` | site, `http` (an own function, already followed) |
