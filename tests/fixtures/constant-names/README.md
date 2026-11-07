# constant-names

Operation names the code states through a constant the compiler reads but
does not type as one string (carrick#3479): a `let` never reassigned, enum
and const enum members, a computed key over a constant, and the string
built-ins (`join`, `concat`, `replace`, `replaceAll`, `String.raw`) and
templates over such parts. One file per form, across HTTP calls, routes,
pub/sub topics and socket events.

`rows.expected.json` is the answer key: `names` gives the one name each
file's site forces and the scan writes as a fact row (a `kind_asked`
pub/sub file waits on the broker's kind question instead); `same_as_const`
pins that a `let` base and a `const` base, a path or a host, give the same
row; `unnamed` lists the files where
a part does not fold (a `let` assigned again, an object written through, a
replacement read at runtime), which must write no fact row and stay leads.
