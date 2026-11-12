/**
 * Exact-version dependency pins for the stub package, from two sources:
 *
 *  1. `installedVersions` — the repo's own node_modules, when installed.
 *     This is the version the repo actually resolves against, so it wins
 *     over the lockfile, and it covers repos whose lockfiles we do not
 *     parse at all (yarn classic v1, binary bun.lockb).
 *  2. `lockfileVersions` — parsed lockfile fallback for bare checkouts
 *     (npm v1/v2/v3, pnpm v6/v9, yarn-berry v2+, and bun's text bun.lock
 *     in scope).
 *
 * Pin selection prefers the DIRECT (top-level) dependency version: a package
 * present at multiple versions must pin to the version the emitted tree
 * resolves against — the hoisted/direct install — never to whichever nested
 * copy happens to appear first in the lockfile (npm sorts
 * `node_modules/a/node_modules/b` before `node_modules/b`, so first-match
 * picks a nested version the tree never sees). When a lockfile offers
 * multiple versions and no direct install can be identified, the package
 * stays UNPINNED (fail-closed abstain): a wrong pin fails or corrupts the
 * synthetic-workspace typecheck, no pin merely abstains.
 */
/**
 * Published-semver gate for node_modules pins. Rejects anything a registry
 * install could not satisfy: workspace/link protocol leakage
 * (`workspace:*`, `link:...`) and yarn-berry's `0.0.0-use.local` local
 * sentinel. A rejected version means "unpinned" (fail-closed abstain
 * downstream), never a pin that would fail the synthetic-workspace install.
 */
export declare function isPublishedSemver(version: string): boolean;
/**
 * Resolve exact versions from the repo's installed node_modules by reading
 * `node_modules/<pkg>/package.json` for each referenced external. On an
 * installed checkout this is ground truth — the versions the repo actually
 * runs against — and it needs no lockfile parser, which is what covers the
 * formats without one (yarn classic v1, binary bun.lockb).
 *
 * Workspace-member guard (fail-closed): package managers link workspace
 * siblings into node_modules as symlinks (`node_modules/@ws/member ->
 * ../../packages/member`). Those carry unpublished versions; pinning one
 * would make the whole synthetic-workspace install fail at check time. So a
 * pin is taken only when the entry's realpath still lives under a
 * node_modules directory inside the repo. pnpm's own
 * `node_modules/<pkg> -> node_modules/.pnpm/<pkg>@<v>/node_modules/<pkg>`
 * links pass that test and stay pinnable; realpaths inside the repo but
 * outside every node_modules (workspace members) and realpaths outside the
 * repo entirely (npm/yarn link to a dev checkout) are skipped — the
 * external stays unpinned unless the lockfile supplies a version.
 */
export declare function installedVersions(repoRoot: string, names: Iterable<string>): Map<string, string>;
/**
 * Resolve exact versions for the repo. Walks up from repoRoot so monorepo
 * members with a hoisted root lockfile still pin (nearest lockfile wins;
 * package-lock.json > pnpm-lock.yaml > yarn.lock > bun.lock at the same
 * level).
 */
export declare function lockfileVersions(repoRoot: string): Map<string, string>;
