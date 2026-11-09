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
