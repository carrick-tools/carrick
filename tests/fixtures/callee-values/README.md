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
