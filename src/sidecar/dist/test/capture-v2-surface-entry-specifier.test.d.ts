/**
 * The surface entry names an anchor's module in a form the entry resolves
 * (carrick#1911).
 *
 * The entry is a `.ts` file written inside the service, and every `symbol`
 * and `handler_return` anchor is an import type of its declaring module:
 * `import('<specifier>').Name`. The specifier was the module's path with its
 * extension removed. TypeScript resolves that form only where the module
 * format allows a bare path: under `module` `node16`..`nodenext`, a file in a
 * `"type": "module"` package must name the output file (`./parcels.js`), so
 * the import did not resolve, the module never joined the declaration emit,
 * and every such alias self-checked "dangling internal specifier". A `.mts`
 * or `.cts` module has no bare-path form in any mode, so its anchors dangled
 * everywhere.
 *
 * The output file's name resolves in every mode: `.js` for `.ts`/`.tsx`,
 * `.mjs` for `.mts`, `.cjs` for `.cts`. The entry uses it where the bare path
 * does not resolve, and keeps the bare path where it does, so a service that
 * captured before captures the same surface.
 */
export {};
