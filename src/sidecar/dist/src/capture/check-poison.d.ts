/**
 * #438 part 2: stub-tree poison containment.
 *
 * A diagnostic in a service's own stub tree (a broken surface line, a nested
 * declaration file that fails to typecheck) used to poison the WHOLE service —
 * every pair with that service as producer/consumer read `unverifiable`
 * (gate=poison:*), masking every clean pair behind one bad alias. This module
 * contains poison to the aliases actually reachable from the poisoned file:
 *
 *  - a diagnostic on the surface entry is attributed to the alias whose
 *    `export type` statement SPAN covers the diagnostic's line (a multi-line
 *    alias reports on an interior line);
 *  - a diagnostic in a nested tree file is attributed to every alias whose
 *    import closure (surface import-type seeds, then BFS over relative
 *    imports — the same closure the capture self-check computes) includes that
 *    file.
 *
 * A stub-tree diagnostic reachable from NO alias (e.g. a cross-stub
 * `declare global` TS2717 collision in an augmentation file, which
 * `skipLibCheck:false` exists to surface — see check-workspace.ts) has no
 * closure to contain it and genuinely can affect any alias's comparison, so it
 * falls back to service-wide poison. Soundness (never falsely compatible) wins
 * over precision in that one case; every attributable diagnostic is contained.
 *
 * Seam: node builtins + `typescript` + this bundle only.
 */
import type { AssembledWorkspace } from './check-workspace.js';
export interface StubPoisonIndex {
    serviceName: string;
    /** Workspace-relative surface key: `packages/<dir>/types/surface.d.ts`. */
    surfaceFile: string;
    /** Every alias declared in the surface. */
    allAliases: Set<string>;
    /** Alias whose `export type` statement span covers the given surface line. */
    aliasAtSurfaceLine(line: number): string | undefined;
    /** Aliases whose import closure includes the given workspace-relative file. */
    aliasesForFile(fileKey: string): string[];
}
/** One index per assembled stub, keyed by service name. */
export declare function buildPoisonIndexes(ws: AssembledWorkspace): Map<string, StubPoisonIndex>;
