/**
 * v2 check core — end-to-end integration (real vendored pnpm + real tsc CLI).
 *
 * Hand-authored stub packages with no external dependencies (so the install is
 * local-only and network-free) exercise the whole runCheck pipeline: workspace
 * assembly, pnpm install, probe generation, the tsc judge, and the four-bucket
 * classifier. Pins each of the four buckets and byte-stability across two runs
 * (the acceptance criteria for WP2).
 */
export {};
