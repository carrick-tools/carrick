/**
 * carrick#2445 (slice 2): library claims on a global the runtime's types
 * package declares. The key is `node:<file under the types package, no
 * .d.ts>` (`node:web-globals/fetch`), the key the scanner computes for a
 * declaration in the runtime's types; its exports are the globals that file
 * declares.
 *
 * - The verifier reads a global as `const local = globalThis.<name>`, and its
 *   home is that file: a global the service adds in its own `declare global`
 *   block is not the runtime's.
 * - The lister lists the value globals that file declares, capped as a
 *   package is, and narrowed by `exports`.
 * - A `node:` key that names a module (`node:events`) is read as before.
 *
 * The runtime types package is vendored and minimal; the service is
 * hand-written.
 */
export {};
