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

## The language's `call` and `apply` (carrick#3504 S4d)

`f.call(t, a)` and `f.apply(t, [a])` run `f` with the arguments shifted past
`this`, where the compiler's row names the member as the language library's.
Discovery's test `a_call_or_apply_fills_the_shifted_parameter` reads the
call's discovery record and the name its decided site states.

| File | Line | Call | Recorded | Name |
|---|---|---|---|---|
| `m-call.ts` | 4 | `http.post.call(http, "/p/m-call", null)` | site, `http` | `/p/m-call` |
| `m-own-call.ts` | 8 | `send.call(null, "/p/m-own-call")` | site, `http` | `/p/m-own-call` |
| `m-own-apply.ts` | 8 | `send.apply(null, ["/p/m-own-apply"])` | site, `http` | `/p/m-own-apply` |
| `m-publish-call.ts` | 8 | `pub.call(null, "m-publish-call")`, `pub` calls `bus.publish` | site, `bus` | `m-publish-call` |
| `m-rpc-call.ts` | 12 | `rpc.call("/p/m-rpc-call")`, a class's own `call` method (control: no shift) | site, `http` | `/p/m-rpc-call` |

## Bound arguments (carrick#3504 S4d-b)

`f.bind(t, a)` binds `a` ahead of the arguments of the call that runs the
result: the call's own arguments fill the parameters after the bound ones.
Discovery's test `bound_arguments_fill_the_leading_parameters` reads the
call's discovery record and the name its decided site states. A `bind` whose
bound arguments are not all text outside any parameter, or a `bind` of a
`bind`, is not read.

| File | Line | Call | Recorded | Name |
|---|---|---|---|---|
| `m-bound-http.ts` | 9 | `send.bind(null, "/p/m-bound-http")`, then `g()` | site, `http` | `/p/m-bound-http` |
| `m-bound-publish.ts` | 9 | `pub.bind(null, "m-bound-publish")`, then `g()` | site, `bus` | `m-bound-publish` |
| `m-bound-split.ts` | 9 | `send.bind(null, "/p/m-bound-")`, then `g("split")` | site, `http` | `/p/m-bound-split` |
| `m-own-bind-this.ts` | 9 | `send.bind(null)`, then `g("/p/m-own-bind-this")` (control: nothing bound) | site, `http` | `/p/m-own-bind-this` |
