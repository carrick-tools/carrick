/**
 * carrick#1605: a symbol the model names through a barrel failed the bundle's
 * symbol check.
 *
 * The model reports a type where the consumer imports it from, and a consumer
 * usually imports from a package's index: `import type { Me } from
 * "./core/index.js"`, where the index only has `export * from ...`. The bundle
 * looked for a declaration of that name in the named file itself, so every
 * such row logged `Symbol 'Me' not found` and lost its explicit anchor.
 *
 * The name is now followed the way an import of it is: through `export *`
 * (any depth), `export { X } from` and `export { X as Y } from`, to the
 * declaration it resolves to. A name two `export *` sources both provide is
 * ambiguous (TypeScript reports TS2308 and an ES module exports neither), so
 * it stays a symbol failure rather than a silent pick of the first.
 */
export {};
