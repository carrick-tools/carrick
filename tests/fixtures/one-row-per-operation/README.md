# one-row-per-operation

A call line states one operation however many texts the code writes for it
(carrick#3984): a `let` written again, a conditional over a route with and
without a query, a fragment. `rows.expected.json` is the answer key: the
calls at each line, as a list (a repeat is a defect), and the shapes that
must stay as they are (two routes at a line are two rows; two fetches of one
route on one line are one).

`__llm__/analyze-file/written.json` is empty so the mock invents no model row.
