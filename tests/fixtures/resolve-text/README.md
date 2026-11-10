# Value resolver: the text reader (carrick#3504 S2)

`repo/services/web` has one service. `src/request_summary/resolve/text_reader.rs`'s
test `the_text_reader_reads_every_form_the_name_fold_read` reads the initialiser
of each `p_*` constant through `Resolver::text_values`: the one text it
always is, with the files whose declarations it read. The `p_*` names are the
probe names; the declarations they read sit above them.

## Answer key

Read (`forms.ts`; every declaration is in this file, so `read_from` is
`src/forms.ts`, except `p_join_default` and `p_replace_all`, which are
literals and read no declaration):

| Probe | Text |
|---|---|
| `p_const` | `/api` |
| `p_let` (a `let` never assigned again) | `/late` |
| `p_enum` | `/items` |
| `p_const_enum` | `/health` |
| `p_entry` (an entry of a constant object) | `/things` |
| `p_computed_key` (`ROUTES[KEY]`, a constant key) | `/other` |
| `p_as_const` (the compiler types the entry as one literal) | `/frozen` |
| `p_join` | `/api/items/all` |
| `p_join_default` (no separator is a comma) | `a,b` |
| `p_concat` | `/api/x/y` |
| `p_replace` | `/v1` |
| `p_replace_all` | `/b/b` |
| `p_raw` | `/api/\d` |
| `p_template` | `/api//items/tail` |
| `p_plus` | `/api//health` |
| `p_local_join` (a `const` in a top-level function's body) | `/api/local` |

A `let` assigned again (`p_written`) is each value it holds, `/before` and `/after` (S2b-4).

Not read (`refuse.ts`, `evalfile.ts`): each is a hole.

| Probe | Why |
|---|---|
| `p_written_entry` | an entry of the object is assigned |
| `p_exported` | an importer may write the object's entries |
| `p_var` | a `var` |
| `p_pushed` | a call may change the object through an entry |
| `p_dollar` | a replacement that names a `$` pattern reads the match |
| `p_param` | a parameter |
| `p_eval` | `eval` in the module may write any binding |

`shadow.ts`: `p_shadow` reads `/outer`: the inner function declares its own
`OUTER` and writes its entry, which does not block the outer one.

Choices (`alt.ts`, carrick#3504 S2b-1): `?:`, `??` and `||` read every branch, never collapsed. `p_coalesce` is a hole (`maybe`, a runtime value) and `/fallback`; `p_cond` is `/api` and `/other`; `p_chain` is a hole, `/api` and `/last`; `p_union` is `/u/a`, `/u/b` and `/u/c`.

`selfread.ts`: `p_self` reads itself; the second read is a recursion hole, and
reading it again gives the same answer (the mark is released).

## Parameters and the calls that fill them (`callers.ts`, carrick#3504 B10-1)

Each `p_*` probe is a call of a `declare function p_<case>(name: string)`;
the function that makes it is written above and called at the bottom. A
parameter is in the file's parameter table beside the binding table (never in
it), by the line and name it is declared on; each call keeps every argument
as the text it states.

Parameter table (function, position, keys):

| Parameter | Entry |
|---|---|
| `one`'s `name` | `one`, 0, no keys |
| `keyed`'s `topic` | `keyed`, 0, `topic` |
| `keyedDefault`'s `topic` | `keyedDefault`, 0, `topic`; its default `jobs.default` is kept as text at those keys |
| `pair`'s `prefix`, `name` | `pair`, 0 and 1 |
| `defaulted`'s `name` | `defaulted`, 0; not written, not colliding (its own default entry in the binding table is not another binding) |
| `Bus.send`'s `topic` | `Bus.send`, 0 |
| the `forEach` callback's `item` | no function (written inside a body), 0 |
| `written`'s `name` | written: the body assigns it again |
| `picked`, a callback's parameter on the line of a `const picked` | collides; the `const` stays placeable |

Call arguments (`CallIr::arg_texts`):

| Call | Texts |
|---|---|
| `two("order.created")` | `order.created` |
| `two("order.updated")` | `order.updated` |
| `two(...names)` | none: a spread ends the list |

What a later reader answers for each probe is that ticket's (B10-1b) key.
Expected, read through callers: `p_one` `order.created`; `p_two` `order.created`
and `order.updated`; `p_keyed` `jobs.run`; `p_member` `member.topic`;
`p_defaulted` `fallback` and `given`; `p_pair` `a:x` and `b:y` only; `p_outer`
`deep` through two calls; `p_loop` `once`; `p_ping` `m`; `p_grow` `seed` and a
recursion hole; `p_joined` and `p_lookup` a hole (a part of a larger text never
takes a caller's text); `p_uncalled` no caller; `p_callback` a callback
parameter; `p_written` reassigned; `p_runtime` two holes, one per caller;
`p_send` `bus.sent`.

Read through callers (`Resolver::texts_through_callers`, B10-1b), each value
carries the calls it was read through, innermost first:

| Probe | Values (text or hole word, then its calls) |
|---|---|
| `p_one` | `order.created` at `one("order.created")` |
| `p_two` | `order.created` at `two("order.created")`; `order.updated` at `two("order.updated")`; `caller_arg_missing` at `two(...names)` |
| `p_outer` | `deep` at `outer(name)`, then `middle("deep")` |
| `p_pair` | `a:x` at `pair("a", "x")`; `b:y` at `pair("b", "y")` |
| `p_keyed` | `jobs.run` at `keyed({ topic: "jobs.run" })` |
| `p_keyed_default` | `jobs.default` (the key's default) at `keyedDefault({})` |
| `p_member` | `member.topic` at `member({ topic: "member.topic" })` |
| `p_defaulted` | `fallback` (the default) at `defaulted()`; `given` at `defaulted("given")` |
| `p_uncalled` | `name_never_bound`, no call |
| `p_callback` | `callback_parameter`, no call |
| `p_written` | `reassigned`, no call |
| `p_shared` (a parameter on the line of a `const` of its name) | `reassigned`, no call |
| `p_loop` | `once` at `loop("once")`; the call `loop(name)` adds nothing |
| `p_ping` | `m` at `ping("m")`; the `ping`/`pong` cycle adds nothing |
| `p_grow` | `seed` at `grow("seed")`; `recursion` at `grow(name + "/x")` |
| `p_joined`, `p_lookup` | `unread_expression`, no call: a part of one text never takes a caller's text |
| `p_runtime` | two holes, one at each `runtime(...)` call |
| `p_send` | `bus.sent` |

Read without callers (every other entry point), a parameter answers as
before: `p_one` is `parameter_unread`, `p_defaulted` is `unread_expression`
and `fallback`.

## The parts of a name and their pieces (`pieces.ts`, carrick#3504 B10-2a1)

Each `p_*` probe's argument is a name whose parts the text lowering spans;
`FileIr::arg_pieces` keeps, at each part's span, the pieces the request
summaries read it as. `base.ts` declares the imported base.

| Probe | Part | Term | Pieces at its span |
|---|---|---|---|
| `p_url` (in `user(id)`) | `${id}` | a read | `Param(0, "id")` |
| `p_env` | `${process.env.API_URL ?? "http://localhost"}` | a choice, spanning the `??` | one environment read of `API_URL`, its fallback `http://localhost` |
| `p_built` (in `built(q)`) | `${encodeURIComponent(q)}` | unlowered, spanning the call | one hole, `unread_expression`, written `encodeURIComponent(q)` |
| `p_base` | `${API_BASE}` | a read | `https://api.example.test`, once every module is linked |
| `p_local` (in `local(id)`) | `url`, a `const` of the body | a read | `/orders/` then `Param(0, "id")`: the local read in place |
| | its initialiser's `${id}` | a read | `Param(0, "id")` |

`itemsApi` is an exported object the module writes through
(`itemsApi.label = …`), so its member `replace` is a function the
module-level code writes, no function of the file. Its parameter `id` is
keyed by the definition `itemsApi.replace` (position 0, no keys, not
colliding), as the caller index keys the function (carrick#2793).

## A name read as pieces through callers (`pieces.ts`, carrick#3504 B10-2a2)

`Resolver::pieces_through_callers` reads a probe's argument as parts, one
value per caller chain. A part is a literal or a hole, with where it was read
and the calls of the parameters it was passed through (innermost first).

| Probe | Values |
|---|---|
| `p_url` (in `user(id)`) | `/users/` then `42`, passed through `user("42")`; `/users/` then an `unread_expression` hole at `req.params.id`, passed through `user(req.params.id)` |
| `p_env` | one hole, `unread_expression`, at the `??` choice, then `/items` |
| `p_base` | `https://api.example.test` at the `API_BASE` read, then `/things` |
| `p_typed` (in `typed(kind: "jobs.run")`) | `jobs.run`, passed through `typed("jobs.run")`; read whole, `jobs.run` through no call |
| `p_wide` (in `wide(name)`) | `w01` to `w17`, each passed through its own call: seventeen values, no cap |
| `p_local` | `/orders/` then a `name_never_bound` hole at the initialiser's `id`: the summaries read `url` as its initialiser |
| `p_target` (`const t = process.env.API_URL + id`) | one part at the read of `t`: the summaries read `t` by its name |
| `p_renamed` (`const u = opts.url`) | one part at the read of `u`: the summaries read `u` by its name |

Read at the labelled call's own function, each `p_local`, `p_target` and
`p_renamed` value spells what the summaries read at the call's argument.

## Block and nested locals (`blocks.ts`, carrick#3794)

The binding table holds a declaration below a body's top level and in a
function written inside a body, under the scope that owns it. Each probe is a
call of a `p_*` function; every literal is annotated `string`, so the
compiler's literal type does not answer before the table does.

| Name | Entry |
|---|---|
| `inIf`, `inTry`, `inCase`, `bare`, `inLoop`, `moved` | a block of `blocks`; `moved` is written again, and the write is placed |
| `atModule` | a block at module scope |
| `inCallback`, `inHandler` | a function written inside a body, top level (text only) |
| `deeper` | a block of such a function |
| `nestedMoved` | such a function's `let` written again: its write is never placed |
| `inner`, `innerInCallback` | a function declared in a body, in `blocks` and in a callback |
| `pair` (two blocks, one line) | one entry, unplaceable |
| `item` (loop heads), `err` (`catch`), `same` (declared twice), `n` (a nested parameter's default) | no entry |

No reader reads an entry of these classes yet: every probe above still reads
as before (`outside_service`). The steps that open each reader replace that
answer.
