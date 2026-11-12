/**
 * Regression for carrick#1742: the capture's dangling-import repair rewrote
 * the scanned repo's own source files.
 *
 * The self-check links the repo's `node_modules` into the stub so that the
 * emitted declarations resolve their dependencies. In a workspace, one of
 * those dependencies is a sibling package linked into `node_modules`, and its
 * `types` can be the package's TypeScript SOURCE. The self-check program then
 * loads the repo's own files. When one of them imports a module the checkout
 * does not have (a generated client that was never generated), the compiler
 * reports it missing in THAT file, and the repair (carrick#1397) dropped the
 * import and wrote `unknown` over its names, on disk, in the user's tree.
 *
 * The repair exists for the declarations the capture emitted into its own
 * stub. A file outside the stub is the user's, and a scan reads it, never
 * writes it.
 */
export {};
