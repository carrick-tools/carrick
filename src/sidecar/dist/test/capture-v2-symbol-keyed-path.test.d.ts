/**
 * A member keyed by a unique symbol is named the way the source writes it,
 * the same on every scan (carrick#1766).
 *
 * Such a member has no name of its own. The checker calls it
 * `__@<description>@<symbol id>`, and the id counts every symbol the process
 * made before it, so it differs between two scans of one tree. The deep walk's
 * path reaches the stored record (`any_provenance`) and, through the check
 * phase's pre-gate, a stored verdict's diagnostic, so an id in it made an
 * unchanged tree read as changed.
 *
 * Two programs over the same files stand in for two scans: the compiler
 * numbers symbols per process, so the second program's ids come after the
 * first's.
 */
export {};
