/**
 * v2 capture — all anchor forms over the stdio wire, on one bare and one
 * installed fixture (the WP1 acceptance matrix).
 *
 * Bare fixture (capture-v2-bare, no node_modules):
 *  - symbol anchors for HTTP response, HTTP request, and GraphQL op types
 *  - handler_return with the design-doc guards: the good path emits
 *    Awaited<ReturnType<...>>; overload sets and generics demote with a
 *    recorded reason (never a silently wrong .d.ts)
 *  - pub/sub payload via the symbol path through a missing external —
 *    the amendment-1 allowlist (pinned in lockfile, kept at tier)
 *  - pub/sub payload via the infer path (object-literal publish argument)
 *    printed by the SymbolTracker-backed node builder
 *  - the honest limitation: inference THROUGH the missing library bakes
 *    `any` in the emitted tree (allowlisted; probe gates are the backstop)
 *  - tsconfig-paths rewrite: `@app/*` specifiers become tree-relative
 *  - augmentation shipping: declare-global and module-augmentation files
 *    ride the tree as extra emit roots
 *  - lockfile pruning: deps never referenced by the tree are not pinned
 *
 * Installed fixture (capture-v2-installed, committed node_modules with
 * @acme/models):
 *  - self-check resolution through the repo's node_modules (symlinked into
 *    the stub for the check): externals resolve, aliases classify ok
 *  - node-builder printing of an external package type as
 *    import("@acme/models").WidgetDto
 *  - exact installed-node_modules pins for scoped packages: the fixture's
 *    package-lock.json disagrees on purpose (2.0.0) and the installed
 *    version (2.1.0) must win
 */
export {};
