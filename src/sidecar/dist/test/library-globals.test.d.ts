/**
 * carrick#2445: library claims on the language's own library. A global the
 * default library declares has no specifier, so its claims name it under a
 * `lib:<lib file>` key (`lib:lib.dom.d.ts`), with the compiler's version as
 * its installed version: the store keys the answer `lib:lib.dom.d.ts@<version>`.
 *
 * - The verifier reads a global as `const local = globalThis.<name>`, and its
 *   home is the default-library file set: a member the service adds in its own
 *   `declare global` block is not the library's.
 * - The lister lists the globals that one lib file declares, when the
 *   program's own `lib` setting loads it, capped as a package is.
 * - A program whose `lib` setting does not load the file answers
 *   `export_missing` for every claim on it, and lists nothing.
 *
 * The service files are hand-written; the declarations are the compiler's own
 * default library.
 */
export {};
