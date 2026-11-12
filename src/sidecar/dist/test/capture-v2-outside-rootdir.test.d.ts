/**
 * A declaration emitted for a source outside the emit's rootDir lands inside
 * the stub (carrick#1770).
 *
 * A service that reads a sibling package's SOURCE (a `paths` mapping or a
 * relative import into `../../packages/...`) puts files outside its rootDir in
 * the capture program. tsc does not emit their declarations under `outDir`:
 * it hands the write callback the source's own path with `.d.ts`. The
 * relocation used to join that onto the stub's `types/` dir by its path
 * relative to the staging dir, which climbs out of the stub. Where the climbed
 * path could not be created the capture failed; where it could, the
 * declaration was written outside the stub and the stub referenced a file the
 * check never copies.
 *
 * The stub must stand alone: copied anywhere, its surface type-checks and its
 * aliases read the sibling package's types.
 */
export {};
