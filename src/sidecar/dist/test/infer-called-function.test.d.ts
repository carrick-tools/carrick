/**
 * carrick#3500 step one: "which function body does this call run" is one
 * answer, read by five helpers. An overloaded function (signatures plus one
 * implementation) and a function wrapped in `satisfies` (`(x) => x satisfies
 * Fn`, `const f = ((x) => x) satisfies Fn`) are the same body as the plain
 * declaration, so every helper finds it.
 *
 * The helpers are private to their classes; the tests reach them by name
 * on a project loaded in process.
 */
export {};
