/**
 * How the scanned repo's installed tree resolved the packages a stub pins
 * (carrick#2091).
 *
 * A stub pins only the externals its declarations import. The check
 * workspace has no lockfile, so without this record every package those pins
 * depend on resolves fresh from the registry: the check can install a version
 * the repo never ran, or fail on one the registry lists but cannot serve.
 *
 * The walk reads the installed tree the way Node's lookup does, so one reader
 * covers npm, pnpm (isolated store and hoisted), yarn's node-modules linker
 * and bun. Starting at each pin installed at the pinned version, it follows
 * `dependencies` and `optionalDependencies` breadth first and records
 * `<parent>@<version> -> <child>: <version>` for every child installed as a
 * published package of that name. Workspace members, `link:` targets and npm
 * aliases are left out: the registry cannot serve them by that name. An edge
 * whose parent version is installed twice with different children is left
 * out, since no one answer is the repo's.
 *
 * The walk only reads. Seam: node builtins and this bundle only.
 */
/** The stub file the check reads to pin transitive edges. */
export declare const RESOLUTION_FILE = "carrick-resolution.json";
/** `<parent>@<version>` -> child name -> child version. */
export type ResolutionEdges = Record<string, Record<string, string>>;
export interface InstalledResolution {
    /** Key-sorted at both levels; byte-stable for one tree. */
    edges: ResolutionEdges;
    /** Dependencies of walked packages that no edge records: the check resolves these from the registry. */
    unrecorded: number;
}
/**
 * The edges the installed tree under `serviceRoot` resolved, from the stub's
 * pins outward. A pin not installed at its pinned version starts no walk.
 */
export declare function installedResolutionEdges(serviceRoot: string, pins: Record<string, string>): InstalledResolution;
