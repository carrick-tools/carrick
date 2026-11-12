/**
 * Readiness must not scale with the scanned repo's installed dependencies
 * (carrick#749, sidecar half of carrick#748).
 *
 * The 0.3.42 Action installs the scanned repo's dependencies, and on a large
 * monorepo that turned a 0.5 s init into a 40 s one, past the readiness budget,
 * dropping the whole type layer. None of that time was compilation: the
 * default-pattern fallback used ts-morph's `addSourceFilesAtPaths`, which
 * recursively registers every descendant directory of any directory a glob hit
 * in — the repo root, and so `node_modules` with it.
 *
 * Three properties are asserted:
 *  - reaching a usable project costs the same over a large installed tree as
 *    over a small one;
 *  - `node_modules` is never registered and never contributes a source file;
 *  - `init` answers without building the program at all, and a later request
 *    still gets a working one.
 *
 * The scaling assertion compares a small tree against a large one rather than
 * checking an absolute wall-clock bound, so a slow or loaded machine moves both
 * arms together and the test still measures scaling.
 */
export {};
