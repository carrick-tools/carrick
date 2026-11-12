/**
 * Scratch synthetic-monorepo assembly for the v2 check phase.
 *
 * Copies each capture stub package into a temp pnpm workspace
 * (node-linker=isolated, so every stub keeps its OWN node_modules and two
 * versions of one dependency genuinely coexist), adds a `carrick-probes`
 * package that depends on every stub via `workspace:*`, writes the checker
 * tsconfig, and computes semver-dedupe overrides so patch/minor drift on a
 * private-member class does not manufacture a nominal false-incompatible
 * (design Check step 6) while genuinely conflicting majors stay physically
 * duplicated (and thus verdict incompatible, correctly).
 *
 * Seam: node builtins + this bundle only.
 */
import type { CheckStubInput } from './api.js';
import type { ProbePlan } from './check-probe.js';
import { WriteGuard } from './guarded-fs.js';
import { type ResolutionEdges } from './resolution-edges.js';
export interface AssembledStub {
    serviceName: string;
    /** Workspace package dir under packages/ (== the @carrick/<dir> suffix). */
    packageDir: string;
    /** Full package specifier, e.g. @carrick/orders-engine. */
    packageName: string;
}
export interface AssembledWorkspace {
    workspaceDir: string;
    /** Every write and the clean-up are held to `workspaceDir` (carrick#1748). */
    guard: WriteGuard;
    probesDir: string;
    /** Relative (forward-slash) probes dir, e.g. packages/carrick-probes. */
    probesRel: string;
    stubs: AssembledStub[];
    /** service name -> package specifier (for probe imports). */
    packageOf: (serviceName: string) => string;
    /** package dir -> package specifier (for scrub labels). */
    packageLabelOf: (packageDir: string) => string | undefined;
    /** package dir -> service name (for poison attribution). */
    serviceOfPackageDir: (packageDir: string) => string | undefined;
}
/**
 * Build pnpm exact-selector overrides that collapse semver-compatible drift to
 * the max version in each compat group, leaving conflicting majors untouched.
 * Returns a deterministically key-sorted map, e.g. { "bson@6.8.0": "6.10.1" }.
 */
export declare function computeDedupeOverrides(stubs: {
    dependencies: Record<string, string>;
}[]): Record<string, string>;
/**
 * pnpm parent-scoped overrides (`<parent>@<version>><child>`) that install
 * each edge the stubs recorded at the version the scanned repo installed
 * (carrick#2091). A parent-scoped override wins over a generic one, so an
 * edge's version first passes through the dedupe table: one physical copy per
 * compat group still holds. An edge two stubs record at versions the dedupe
 * does not reconcile is left to the resolver.
 */
export declare function computeResolutionOverrides(edgeSets: ResolutionEdges[], dedupe: Record<string, string>): Record<string, string>;
export interface AssembleOptions {
    stubs: CheckStubInput[];
    workspaceRoot?: string;
}
/** Create the scratch workspace and copy in the stub packages. */
export declare function assembleWorkspace(opts: AssembleOptions): AssembledWorkspace;
/** Write the generated probe files into the assembled probes package. */
export declare function writeProbes(ws: AssembledWorkspace, plans: ProbePlan[]): void;
export declare const CHECKER_TSCONFIG: string;
