# object-export-surface

carrick#830: a module whose surface is an object literal contributes its
function members to the index, instead of reading as a file with no functions
in it.

`src/edge.js` is the shape the ticket found: `export default { async
fetch(request) { … } }`, plain `.js`, the only function in the file a method on
an object literal. It produced no function definition at all — no signature, no
intent, nothing for anything else in the index to point at — because it is not
a `FnDecl`, not a `VarDeclarator` and not a class member, which were the three
shapes the definition extractor knew.

`src/handlers.ts` is the same surface in its named form: a method, an
arrow-valued property, and one level of nesting. `internals` in the same file is
the bound — an object the module keeps to itself is a value it uses rather than
a surface it offers, and its members stay out of the index.

`src/lambda.js` and `src/queue.js` are the same surface on the other module
system (carrick#863): `exports.handler = …`, `module.exports.health = …`, an
object assigned to a named export, and `module.exports = { … }`, which is the
default export and keys its members `default.<member>` exactly as
`export default { … }` does. Each file also carries the bound that matters
there: a function offered by name (`reset`, `consume`) has ONE definition, at
its own key, because a second row for the same body would double the function
count and re-bill its intent — and `sweep`, which no assignment reaches, stays
unexported.

The LLM is replayed from `__llm__/` and states nothing: what is under test is
what the scanner derives from the AST on its own.
