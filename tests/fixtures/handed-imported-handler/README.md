# handed-imported-handler

A request handler handed on by name from another file (carrick#4176). Every
name is made up. `repo/` is one root service; `library-claims.json` is the
store answer replayed for it: `fixture-router`'s `route`, labelled `receive`
with a `name` (path) slot at argument 0, the handler at argument 1, and the
request's `path` and `method` fields.

- `handlers.ts` exports `stats`, which compares `req.path` with `/stats`.
  Nothing calls it.
- `server.ts` imports `stats` and hands it to `route("/admin", stats)`.

The answer key: `stats` is handed to the `route` call, so it is no entry
point. Its handler record is the registration's: `subject.kind` is `label`
with the `fixture-router` `route` label, not `entry`. This is the
`dispatch-routes` `src/mounted.ts` `stats` shape with the handler imported;
the compiler's row at `stats` in `server.ts` names the definition, which the
syntax of `server.ts` alone cannot.
