/**
 * Position rows for the JavaScript files no owner program takes (carrick#2413).
 *
 * The fixture (`tests/fixtures/position-rows-scripts/`) is a service whose
 * tsconfig has no `allowJs`. Its `.mjs` and `.cjs` files are in no program the
 * service's config builds, so the walk reads them in that config's `scripts`
 * program: the same options with JavaScript accepted. A name a script imports
 * from a TypeScript module or a sibling script, or reads from a global the
 * service's own declaration file declares, is attributed to its declaration.
 * What the owner's program reads is unchanged, and the walk writes nothing but
 * its rows file.
 */
export {};
