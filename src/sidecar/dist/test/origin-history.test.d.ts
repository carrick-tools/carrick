/**
 * carrick#1731, outside Deno: a dependency linked from a directory outside
 * the checkout keeps reading as library origin after the project is edited.
 *
 * ts-morph rebuilt the program after any edit to the project with every file
 * it had loaded as a root file, and the compiler marks no root file as an
 * external-library import. The loader now pins the roots (carrick#3146), so the
 * flag survives on the compiler's own record as well as on the resolver's.
 * A dependency installed under `node_modules` is still recognised by its path. One linked from outside the checkout (`npm
 * link`, a `link:` or `file:` dependency) resolves to its real path, which has
 * no `node_modules` segment, so the compiler's flag was the only thing that
 * said it was a library, and the first edit made the printer inline it.
 */
export {};
