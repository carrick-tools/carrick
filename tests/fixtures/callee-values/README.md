# Callee values held in module bindings (carrick#3504 S1, carrick#3485 rows 1-2)

`repo/services/web` has one service. `src/send.ts` declares `send` and
`sendToo`, which call the global `fetch` (the one label in
`library-claims.json`), and `other`, which sends nothing. Each `f-*.ts` file
calls a function held in a module-scope binding. Discovery's test
`a_callee_held_in_a_module_const_or_let_is_each_value_written_to_it` reads
the call in `f`.

A module binding holds its initialiser and every value an assignment writes
to it (the decision record's soundness rule 1). A write the census cannot
place (a destructuring target, a write inside a callback) makes the binding
hold something no file states, so the call stays undecided.

## Answer key

| File | Line | Binding | Recorded |
|---|---|---|---|
| `f-direct.ts` | 4 | none: `send(…)` directly (control) | site, `fetch` |
| `f-alias.ts` | 6 | `const s = send` | site, `fetch` |
| `f-let-never.ts` | 6 | `let s = send`, never written | site, `fetch` |
| `f-let-agree.ts` | 10 | `let s = send`, `swap` writes `sendToo` | site, `fetch` (both values reach it) |
| `f-let.ts` | 10 | `let s = send`, `swap` writes `other` | `several_values` |
| `f-let-param.ts` | 10 | `let s = send`, exported `set(next)` writes its parameter, nothing calls `set` | `several_values` |
| `f-let-unplaced.ts` | 10 | `let s = send`, `[s] = [sendToo]` | `several_values` |
| `f-let-nested.ts` | 12 | `let s = send`, written inside a `setTimeout` callback | `several_values` |

## Function-local `let` (carrick#3504 S1b, carrick#3485 row 2)

Each `f-local-*.ts` file calls a function held in a `let` its own function
declares and writes again. The binding table reads it the way it reads a
module binding: its initialiser and every write the census places, a value
of the function's parameter read at that function's callers. Discovery's test
`a_callee_held_in_a_function_local_let_is_each_value_written_to_it` reads the
call at the line below.

| File | Line | Binding | Recorded |
|---|---|---|---|
| `f-local-agree.ts` | 8 | `let s = send`, `if (flag) s = sendToo` | site, `fetch` (both values reach it) |
| `f-local-assigned.ts` | 6 | `let s;` then `s = send` | site, `fetch` |
| `f-local-param-called.ts` | 8 | `let s = send`, `if (flag) s = next`; the one caller passes `sendToo` | site, `fetch` |
| `f-local-init-param.ts` | 8 | `let s = next`, `if (flag) s = send`; the one caller passes `sendToo` | site, `fetch` |
| `f-local-let.ts` | 8 | `let s = send`, `if (flag) s = other` | `several_values` |
| `f-local-param.ts` | 8 | `let s = send`, `if (flag) s = next`; nothing calls the exported `f` | `several_values` |
| `f-local-nested.ts` | 8 | `let s = send`, written inside a `setTimeout` callback | `several_values` |

## A callee read off a kept object's entry (carrick#3504 S4b)

Each file below calls the entry of an object literal the module (or the
function) keeps: `d["a"]()`, `d[k]()` for a `const` key, `d.a()`. The entry is
the value the call runs, where the object is a `const` nothing writes
through, an exported one no importer writes through included (the
cross-module check of carrick#2448). An object the module or an importer may
change and an open object (a spread may put any key in it) keep the call
undecided. Discovery's test
`a_callee_read_off_a_kept_objects_entry_is_that_entrys_value` reads the call
at the line below.

| File | Line | Read | Recorded |
|---|---|---|---|
| `f-keyed.ts` | 6 | `d["a"](…)`, `d = { a: send }` | site, `fetch` |
| `f-keyed-const.ts` | 7 | `d[k](…)`, `const k = "a"` | site, `fetch` |
| `f-entry-dot.ts` | 6 | `d.a(…)` | site, `fetch` |
| `f-entry-method.ts` | 10 | `d.a(…)`, `{ a(p) { return send(p); } }` | site, `fetch` |
| `f-local-keyed.ts` | 5 | `d["a"](…)`, `d` a function-local `const` | site, `fetch` |
| `f-keyed-written.ts` | 10 | `d.a = other` elsewhere | not decided |
| `f-keyed-exported.ts` | 6 | `export const d`, no importer writes it | site, `fetch` |
| `f-keyed-contested.ts` | 6 | `export const d`, `f-keyed-contested-writer.ts` writes `d.a` | not decided |
| `f-keyed-spread.ts` | 5 | `{ a: send, ...x }` | not decided |

## A hop the rows resolve (carrick#3504 S4b-3b)

Each file below calls a function held in an object entry under another name
or a computed key. The call graph has no edge to the function; the rows do.
The decided-sites test
`a_hop_the_rows_resolve_is_placed_without_a_call_graph_edge` reads the call
at the line below.

| File | Line | Read | Recorded |
|---|---|---|---|
| `f-own-renamed.ts` | 8 | `d.go(…)`, `d = { go: send2 }` | site, `/p/f-own-renamed` |
| `f-own-keyed.ts` | 8 | `d["send2"](…)`, `d = { send2 }` | site, `/p/f-own-keyed` |
| `f-exported-arrow-keyed.ts` | 6 | `d["a"](…)`, `export const d = { a: (p) => fetch(p) }` | site, `/p/f-exported-arrow-keyed` |

## Instance fields (carrick#3504 S4c)

A field holds its property initialiser and every `this.f = value` any member
writes, the constructor's included; `this.g` read in one of them is the field
`g`. A class that hands `this` out leaves a field unread when only an initialiser
or a member writes it (a field the constructor sets keeps its reading, S5's
R3 shape: counted as `fields_this_escaped`, re-ruled with data); so does a
write of a member's own parameter. Discovery's test
`a_callee_held_in_a_field_is_its_initialiser_and_every_write` reads the call
at the line below.

| File | Line | Field | Recorded |
|---|---|---|---|
| `f-field-init.ts` | 7 | `f = send` | site, `fetch` |
| `f-field-copy.ts` | 8 | `g = send; f = this.g` | site, `fetch` |
| `f-field-ctor-copy.ts` | 13 | ctor `this.g = send; this.f = this.g` | site, `fetch` |
| `f-field-method-agree.ts` | 11 | `f = send`, a method writes `sendToo` | site, `fetch` |
| `f-field-method-write.ts` | 15 | ctor `this.f = send`, a method writes `other` | `several_values` |
| `f-field-param.ts` | 11 | `f = send`, a method writes its parameter | `several_values` |
| `f-field-escape.ts` | 15 | ctor `register(this); this.f = send` | site, `fetch` |
| `f-field-escape.ts` | 23 | `register(this)` in the ctor; only `setup()` writes `g` | undecided (not a site) |

## An alias of an object's entry (carrick#3504 S4b-2)

A name that holds `d.a` or `d["a"]` is that entry's value, read as the direct
`d.a(…)` is above. Discovery's test
`a_callee_held_in_an_alias_of_an_entry_is_that_entrys_value` reads the call at
the line below.

| File | Line | Read | Recorded |
|---|---|---|---|
| `f-alias-dot.ts` | 7 | `const g = d.a; g(…)`, `d = { a: send }` | site, `fetch` |
| `f-alias-keyed.ts` | 7 | `const g = d["a"]; g(…)` | site, `fetch` |
| `f-alias-local.ts` | 7 | the alias inside `f` | site, `fetch` |
| `f-alias-into-object.ts` | 7 | `h = { post: o.post }; h.post(…)`, `o = { post: send }` | site, `fetch` |
| `f-alias-written.ts` | 11 | `d.a = other` in an exported function | not decided |
| `f-alias-spread.ts` | 6 | `{ a: send, ...x }` | not decided |
