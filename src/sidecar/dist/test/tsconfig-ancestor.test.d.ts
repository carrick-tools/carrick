/**
 * carrick#1776: a service whose tsconfig sits in an ancestor directory, with
 * no tsconfig named for it, was typed with the sidecar's default options.
 *
 * Both readers looked for a config in the service root only. A layout that
 * keeps one tsconfig above several client services (their module resolution,
 * `paths`, `lib`) got none of it: `paths` imports and the libraries the
 * config's resolution finds never resolved, and every type read off them was
 * `any`.
 *
 * A service with no config of its own now takes the nearest `tsconfig.json`
 * above it, found the way `tsc` finds one, walking up from the service root
 * and stopping at the scan root the caller names. Both readers take the same
 * answer:
 *  - the init'd project (`infer`, `bundle`, `retype_check`);
 *  - capture (`capture_v2`), the served surface.
 * Without a scan root, only the service root is searched.
 */
export {};
