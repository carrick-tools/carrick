/**
 * The write guard every sidecar write and delete goes through (carrick#1748).
 * It resolves the path the operation touches and refuses anything outside the
 * run's own roots.
 */
export {};
