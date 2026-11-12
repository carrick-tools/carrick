/**
 * An anchor whose file lies outside the service root arrives as an ABSOLUTE
 * path (the scanner leaves a path outside the capture root as it is). The
 * capture joined every `source_file` onto the root with `path.join`, which
 * nests `/abs` under the root, so the file was "not in program" and the alias
 * demoted. A source file resolves with `path.resolve`: an absolute path stays
 * absolute and a relative one is unchanged.
 */
export {};
