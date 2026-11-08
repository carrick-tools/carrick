# name-forms

Operation names written in a form the names lowering and the text reader
learn one step at a time (carrick#3504 S2b). One file per form and
protocol: `c-<form>.ts` is an HTTP call, `q-<form>.ts` a pub/sub topic,
`s-<form>.ts` a socket event.

Forms so far:

- `destructure`: a name bound by an object pattern (`const { path } = {...}`,
  `{ p: target, ...rest }`, from a kept `const` object, or as a function's
  own local). `c-destructure-written` is unread on purpose: the object is
  written elsewhere (`ROUTES.items = ...`), so the key's value is not stated.

`rows.expected.json` is the answer key, read by `tests/name_forms_test.rs`:
`names` gives the one name each file's site forces and the scan writes as a
fact row (a `kind_asked` pub/sub file waits on the broker's kind question
instead); each `unnamed` file writes no fact row and stays a possible call.
Never edited to make a test pass.
