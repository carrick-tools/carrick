/**
 * Two invented packages for the lister's bounded print (carrick#3744).
 *
 * `fixture-deep-print` exports a function whose return type is a deferred
 * conditional with no alias: each level names the level below twice, once at
 * `T` and once at `T | number`, so printing it in full doubles at every level
 * and runs the compiler out of heap. `fixture-plain-print` declares only the
 * ordinary export, so the two listings of `plain` can be compared.
 */
export declare const DEEP_LEVELS = 26;
export declare function deepDeclarations(): string;
export declare function plainDeclarations(): string;
/** A probe project in `dir` with both packages installed. */
export declare function writeDeepPrintProject(dir: string): void;
