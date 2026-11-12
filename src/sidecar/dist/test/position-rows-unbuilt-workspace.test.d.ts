/**
 * carrick#1910: in the position rows' process, a package of the checkout
 * whose manifest names build output that was never built is read from its
 * source, whether or not the service config references it.
 *
 * A cloud scan installs a monorepo and never builds it, so a workspace
 * package's `typings` (or `types`, or an `exports` entry) names a file under
 * `build/` or `dist/` that does not exist, and every call on a value it types
 * reads the error type. The rows' process is told it is that process by the
 * `init` field `workspace_source`; only then does its loader seed carrick#2418's
 * reference redirect with the packages the install links into the checkout
 * (a `node_modules/<name>` whose real path is inside the checkout and under
 * no `node_modules`), each through its own `tsconfig*.json` files.
 *
 * Every other process (infer, the compat judge's anchors, capture) reads the
 * package as before: the same fixture, initialised without the field, keeps
 * the error type, and capture still reads it as the top type.
 *
 * The fixture (`test/fixtures/unbuilt-workspace/`) is materialised in a temp
 * dir with the links a workspace install writes:
 *
 *  - `@acme/ledger`: `typings` into `build/main/` (`outDir` `build/main`,
 *    `rootDir` `src`; a second `tsconfig.module.json` writes `build/module`).
 *  - `@acme/flags`: an `exports` map into `dist/esm` and `dist/cjs`, one
 *    config each.
 *  - `@acme/split`: two configs write `dist/index.d.ts` from different
 *    sources, so nothing says which one the entry is: it stays unresolved.
 */
export {};
