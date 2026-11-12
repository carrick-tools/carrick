/**
 * Which tsconfig types a service that names none (carrick#1776).
 *
 * Both readers of a service, the init'd project and capture, ask this one
 * function, so they type the service under the same options.
 *
 * The service root is searched first, for the names a service keeps its own
 * config under. Above it, the search is `tsc`'s own when it is run with no
 * `-p`: the nearest `tsconfig.json` in an ancestor directory. A layout that
 * keeps one config above several services (their module resolution, `paths`,
 * `lib`) is typed under that config, as the repo's own `tsc` types it; without
 * it, every import only that config resolves read `any`.
 *
 * A service-root `jsconfig.json` ranks after those names and before an
 * ancestor `tsconfig.json` (carrick#3497): tsserver types a JavaScript file
 * under the jsconfig beside it, not under a config above. Where no ancestor
 * config exists the answer stays undefined, as before: each file is then
 * typed by `GoverningConfigs` alone, which reads the same jsconfig.
 *
 * The walk stops at the scan root, inclusive: a config outside the scanned
 * tree is never read. With no scan root, or a service outside it, only the
 * service root is searched.
 *
 * The service config is the fallback of the per-file rule
 * (`GoverningConfigs`, carrick#3497): a file a config inside the service
 * lists is typed under that config.
 */
/** Absolute path of the config that types the service, or undefined for none. */
export declare function findServiceTsconfig(serviceRoot: string, scanRoot?: string): string | undefined;
/** The path with every symlink resolved; the path as given when it does not exist. */
export declare function realPath(p: string): string;
/**
 * The config each file of one service is typed under (carrick#1604,
 * carrick#3497 ruling R-N1): the one the TypeScript language service picks.
 * From the file's directory up to the service root, inclusive, each directory's
 * `tsconfig.json`, then the `jsconfig.json` beside it, is asked whether its
 * program, or a project it references, lists the file; the first that does
 * types it (a
 * solution config hands the file to the reference that lists it). A config
 * that lists the file nowhere is passed over. The climb stops after a
 * directory named `node_modules`, as tsserver's does. A file no config inside
 * the service lists falls back to the service config (`findServiceTsconfig`
 * or the named one) and the project of its references that lists the file,
 * else to that config itself; with no service config, to none.
 *
 * The scanner's `module_aliases::Governing` is the same rule, and
 * `tests/module_resolution_parity_test.rs` holds both to the compiler's own
 * answer.
 */
export declare class GoverningConfigs {
    private readonly serviceConfig;
    private readonly report;
    private readonly root;
    /** Directory -> the configs in it, in order, for every directory a climb visited. */
    private readonly dirConfigs;
    /** Config -> its projects; undefined for one that cannot be read. */
    private readonly graphs;
    /**
     * @param serviceRoot the service directory
     * @param serviceConfig the fallback config, absolute, or undefined for none
     * @param report where a config or reference that cannot be read is named
     */
    constructor(serviceRoot: string, serviceConfig: string | undefined, report?: (diagnostic: string) => void);
    /** Absolute path of the config that types `file`, or undefined for none. */
    configFor(file: string): string | undefined;
    private configsIn;
    /** The project reached from `config` that lists `file`, absolute. */
    private listingOwner;
}
