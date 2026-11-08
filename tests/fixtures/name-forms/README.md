# name-forms

Operation names written in a form the names lowering and the text reader
learn one step at a time (carrick#3504 S2b). One file per form and
protocol: `c-<form>.ts` is an HTTP call, `q-<form>.ts` a pub/sub topic,
`s-<form>.ts` a socket event.

Forms so far:

- `destructure` (S2b-3): a name bound by an object pattern (`const { path } = {...}`,
  `{ p: target, ...rest }`, from a kept `const` object, or as a function's
  own local). `c-destructure-written` is unread on purpose: the object is
  written elsewhere (`ROUTES.items = ...`), so the key's value is not stated.
- `coalesce` (S2b-1): `??`, `||` and `?:`. A choice states its values only
  when every side is stated (`*-fallback-stated`: a constant and a literal).
  A side the code does not state (`maybe ?? '/x'`, `maybe` a runtime value)
  keeps the name asked: stating the fallback alone is the initialiser-alone
  error. `c-coalesce-const` (two constants) and `c-union` (a union of
  literals) are several values and stay asked.

- `static-field` (S2b-6): `C.P` where `static P = '…'` is a property of a
  module-scope class. `c-static-written` (`C.P = …` elsewhere) and
  `c-static-exported` (an exported class) are unread on purpose: the value
  may change, so the field's initialiser is not the name.

`rows.expected.json` is the answer key, read by `tests/name_forms_test.rs`:
`names` gives the one name each file's site forces and the scan writes as a
fact row (a `kind_asked` pub/sub file waits on the broker's kind question
instead); each `unnamed` file writes no fact row and stays a possible call.
Never edited to make a test pass.
