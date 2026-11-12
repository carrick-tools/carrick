/**
 * External-dependency pinning from an installed repo's node_modules — the
 * yarn/bun coverage gap. Lockfile parsing covers npm and pnpm only; on a
 * yarn (classic or berry) or bun repo the emitted surface's externals ended
 * up unpinned and every edge through them abstained at check, even though
 * the repo's own node_modules carried the exact installed versions.
 *
 * Pins:
 *  1. installed repo with ONLY a yarn.lock still pins: versions come from
 *     node_modules/<pkg>/package.json (no lockfile parser involved);
 *  2. bare repo + npm lockfile is unchanged (lockfile fallback regression);
 *  3. workspace-member symlink guard: a node_modules entry whose realpath
 *     lands inside the repo but outside every node_modules directory is a
 *     linked-in workspace sibling with an unpublished version — never
 *     pinned from node_modules (pnpm's .pnpm indirection stays pinnable);
 *  4. precedence: when node_modules and the lockfile disagree, the
 *     installed version wins — it is what the repo resolves against;
 *  5. corrupt or missing node_modules/<pkg>/package.json degrades to the
 *     lockfile fallback (or unpinned) without crashing;
 *  6. non-published version strings (workspace:*, 0.0.0-use.local) are
 *     never pins — a pin the registry cannot satisfy would fail the whole
 *     synthetic-workspace install at check time.
 */
export {};
