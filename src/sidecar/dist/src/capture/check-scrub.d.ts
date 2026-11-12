/**
 * Diagnostic scrub for the v2 check phase.
 *
 * Raw tsc diagnostics leak two run-varying / internal things into the most
 * interesting error class (cross-stub conflicts): stub-absolute
 * `import("/tmp/xxxx/packages/orders/types/surface")` paths inside elaboration
 * text, and the probe's `Sent`/`Expected` import aliases in the headline. This
 * pass maps every workspace-absolute path to a stable `@carrick/<service>` (or
 * `pkg@version`) label and rewrites the aliases to the real surface names, so
 * the message is user-facing AND byte-stable across runs with different temp
 * dirs. Real TS text (e.g. TS2741 "... missing in type 'StockLevel'") is
 * preserved intact.
 *
 * Seam: node builtins only; no imports needed.
 */
export interface ScrubContext {
    /** Absolute path of the scratch workspace root (fully removed from output). */
    workspaceRoot: string;
    /** The same root with symlinks resolved, as a tool that prints real paths
     * names it (pnpm, under a macOS temp dir). Absent when there is no tree. */
    workspaceRealRoot?: string;
    /** Workspace package dir (== sanitized service) -> the @carrick/<dir> label. */
    packageLabelOf: (packageDir: string) => string | undefined;
}
/** Replace stub-absolute import paths and any lingering workspace root. */
export declare function scrubPaths(text: string, ctx: ScrubContext): string;
/**
 * Rewrite the probe's `Sent`/`Expected` import aliases (which the compiler
 * prints in the headline) to the real surface alias names. Only quoted forms
 * are touched, so real type names in nested elaboration lines are preserved.
 */
export declare function rewriteAliases(text: string, sentAlias: string, expectedAlias: string): string;
/** Full scrub applied to an incompatible pair's diagnostic message. */
export declare function scrubDiagnostic(text: string, ctx: ScrubContext, sentAlias: string, expectedAlias: string): string;
