# Routes on a module `let` (carrick#3504 S1, carrick#3489 W5)

One service at the repo root; `fixture-server`'s default export makes the
app, as in `receiver-forms-routes`. Each file registers a route on a
module-scope `let` that holds the app. The receiver trace reads the binding
as every value written to it: its initialiser and each assignment.

## Answer key

| File | Binding | Instance ends at the route (line) |
|---|---|---|
| `s-let.ts` | `let app = make()`, exported `set(next)` writes its parameter, nothing calls `set` | root `s-let.ts:2` and `entry_point` (line 4) |
| `s-let-never.ts` | `let app = make()`, never written | root `s-let-never.ts:2` (line 3) |
| `s-let-unplaced.ts` | `let app = make()`, `[app] = [make()]` | `unread` (line 4): a destructuring write is not placed |

A route on `s-let.ts` keeps both ends (the walk's per-end rule): one root
and one entry point, so its row is not placed at a path, and no row loses its
path. `s-let-never.ts` is placed at `/p/s-let-never`.
