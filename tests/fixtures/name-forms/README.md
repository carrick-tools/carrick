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
  error. `c-union` (a branch typed as a union of literals) stays asked:
  the type states that choice, not the code.
- `logical-assign` (S2b-4): `??=` and `||=` are writes the binding holds
  beside its earlier value (`c-logical-assign`, `c-or-assign`, and the
  topic and socket forms; an empty initialiser or none adds no value).
  `c-and-assign` (`&&=`) computes a value no reader states.

- `destr-assign` (S2b-5): `({ a } = obj)` writes the member `obj.a` to `a`,
  a placed write beside the earlier value (an empty initialiser drops out).
  `c-destr-assign-default` (a default) and `c-destr-assign-array` (`[a] = …`)
  are written in a form no reader places and stay asked.
- `conditional` (S2b-8): a name the code states as several whole values
  writes one fact row per value, on every protocol: `?:` over two literals
  (`*-conditional`), over two constants (`c-coalesce-const`), and a `let`
  written with a second value (`c-let-written`). A name typed as a union of
  literals (`c-union-name`) stays asked.
- `static-field` (S2b-6): `C.P` where `static P = '…'` is a property of a
  module-scope class. `c-static-written` (`C.P = …` elsewhere) and
  `c-static-exported` (an exported class) are unread on purpose: the value
  may change, so the field's initialiser is not the name.

- `getter` (S2b-7): `this.url` where `get url() { return '...'; }` is a getter
  of a module-scope class. `c-getter-overridden` (a subclass redeclares it),
  `c-getter-exported` (an exported class) and `c-getter-body` (the body is
  not the one `return`) are unread on purpose: what the getter returns is
  not stated by the class alone.

- `default`: a parameter the caller does not write is its default
  (`function send(p = '/x')`, `send()`). A destructured key's default
  (`send({ path = '/x' })`) is `tests/fixtures/version-segments`'s
  (carrick#3608). `c-default-given` passes the argument and
  names its own value; `c-default-spread` spreads before the position and
  stays unknown.

`rows.expected.json` is the answer key, read by `tests/name_forms_test.rs`:
`names` gives the one name each file's site forces and the scan writes as a
fact row (a `kind_asked` pub/sub file waits on the broker's kind question
instead); `several` gives every name a file's site states, one fact row
each; each `unnamed` file writes no fact row and stays a possible call.
Never edited to make a test pass.
