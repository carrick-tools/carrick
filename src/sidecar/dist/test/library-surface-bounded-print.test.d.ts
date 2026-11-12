/**
 * carrick#3744: a type print that would expand without bound is listed as
 * `<not printed: too large>` instead of running the lister out of heap. The
 * sidecar runs at the library builder's child heap (1630 MB). Both packages
 * are invented (see deep-print-fixture.ts).
 */
export {};
