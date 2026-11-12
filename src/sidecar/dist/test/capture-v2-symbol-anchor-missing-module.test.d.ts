/**
 * Regression for carrick#1397: a SYMBOL anchor whose emitted declaration
 * imports a module that does not resolve was discarded whole.
 *
 * carrick#1377 kept a type whose printed answer named something undeclared,
 * but only on the two PRINT paths. A symbol anchor prints nothing: its surface
 * line is `import('./m').RenderRequest` and the shape lives in the `.d.ts` the
 * compiler emitted for `m`. When that emitted file carries an import the
 * checkout cannot resolve, the stub reports the module missing, the capture
 * records it as a dangling internal specifier, and the scanner's publish gate
 * refuses the alias — so a request type with twenty resolved members published
 * nothing because of one member.
 *
 * The emitted declaration is repaired instead: the import that did not resolve
 * is dropped and every type it bound reads `unknown` at its own position. That
 * is file-granular, so every alias whose closure reaches the file benefits at
 * once, and it is the same statement the source program already makes — the
 * names were TypeScript's unresolved-reference placeholder there too.
 *
 * The pair is still not judged compatible: the deep-decay rule reads the
 * `unknown` and abstains, naming the member. What this buys is the type
 * reaching the index.
 */
export {};
