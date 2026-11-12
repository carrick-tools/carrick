/**
 * A tsconfig project reads each `package.json` once, not once per program
 * rebuild.
 *
 * Under `module` `node16`..`nodenext`, and for every file under
 * `node_modules`, the compiler reads the `package.json` scope above each
 * source file to decide the file's format. It memoises those reads in the
 * module resolution cache its host hands it. The tsconfig project resolves
 * imports through its own `resolveModuleNames` (carrick#1619), and ts-morph
 * forwarded no cache with it, so every program rebuild after an edit (two per
 * retyped call) read and parsed the whole `package.json` chain again: 37% of
 * a retyped call in an 863-file program.
 *
 * The project now hands the compiler one cache that lives as long as the
 * project. Only its `package.json` entries are ever filled: the compiler
 * resolves no module name itself while the host resolves them, so every
 * module answer still comes from the resolution host. The test builds the
 * same project twice, with and without the cache, rebuilds each after edits,
 * and requires the same files, formats, module answers and diagnostics.
 */
export {};
