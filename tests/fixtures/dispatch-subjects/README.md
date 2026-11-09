# dispatch-subjects

How a compared value is written, for the handler records (carrick#3504
S7b-1). Every name is made up. `repo/` is one root service; each file hands
a handler to `node:http`'s `createServer`, which `library-claims.json`
labels `receive` with its `request.url` / `request.method` slots (the same
store answer as `dispatch-routes`). One comparison per file.

Answer key (`a_compared_binding_reads_every_value_written_to_it`, in
`src/request_summary/dispatch.rs`):

| File | Form | Stands |
|---|---|---|
| `s-direct`, `s-const-local`, `s-method-alias`, `s-named-handler`, `s-reexport`, `s-imported-route`, `s-route-table` | the member read in place, or through a `const`, an alias, an import or a table | settled |
| `s-let-later` | `let path; path = req.url;` | settled |
| `s-coalesce` | `const path = OVERRIDE ?? req.url`, `OVERRIDE` a name that holds no request value | settled |
| `s-let-normalised` | `let` set again with `path.slice(0, -1)` | asked |
| `s-split` | `(req.url ?? "/").split("?")[0]` | asked |
| `s-helper-return` | an own helper returns the member | asked |
| `s-this-field` | `this.req.url` in a class the handler constructs | in no record (the comparison is outside the handler) |
| `s-route-let`, `s-module-let` | the literal side is a module `let`, or text built over one, read by the text reader (S7c, `a_compared_constant_the_text_reader_states_is_its_text`) | settled |
| `s-route-let-written` | the module `let` is set again to another route | in no record (two texts: the literal stays unresolved) |
