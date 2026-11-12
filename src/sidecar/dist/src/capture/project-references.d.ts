/**
 * Which project types a file of a service whose tsconfig references others
 * (carrick#1604).
 *
 * A "solution" tsconfig (`"files": []` plus `references`) carries no compiler
 * options: they live in the projects it references. Parsing the named file
 * alone typed every file with none, so `customConditions`, `paths` and the
 * module resolution mode never applied and workspace imports fell through to
 * unbuilt output.
 *
 * TypeScript's editor answers "which project types this file" by searching
 * the named config, then its references depth-first in declared order, for
 * the first project that lists the file and is not a project one of whose
 * own references lists it too (tsserver's default-project rule, carrick#2418:
 * a file a referenced project lists is that project's source, so a solution
 * config whose default include lists every file owns none of them). That
 * project is the file's owner here too, and a file is only ever typed under
 * its owner's options: the loader builds one program per owner it is asked
 * about, and the capture types each anchor in its owner's program. A file no
 * reachable project owns is owned by the named config, which is how it was
 * typed before references were read.
 */
import ts from 'typescript';
/** One parsed tsconfig. */
export interface ServiceProject {
    /** Absolute path of the config file the options came from. */
    configPath: string;
    parsed: ts.ParsedCommandLine;
}
/**
 * The projects a named config reaches and the owner of each file among them.
 * References are read on the first question about a file, and never for a
 * named config that references nothing, which costs one parse, as before.
 */
export declare class ProjectGraph {
    readonly named: ServiceProject;
    /** References that could not be read, one line each. */
    readonly diagnostics: string[];
    private readonly extendedConfigCache;
    private walked;
    private readonly namedFiles;
    private readonly closures;
    /** Throws when the named config cannot be read, as a bare parse would. */
    constructor(configPath: string);
    /** Whether the named config references other projects at all. */
    get hasReferences(): boolean;
    /**
     * The project that owns `file`: the first in search order (the named
     * config, then its references depth-first in declared order; a config
     * reached twice is visited once) that lists it while no project in its own
     * reference closure lists it. The named config when none does.
     */
    ownerOf(file: string): ServiceProject;
    /**
     * `ownerOf` when some project in the graph lists `file`, the named config
     * included; undefined when none does (carrick#3497: a config that lists a
     * file nowhere is passed over for one further up).
     */
    listingOwnerOf(file: string): ServiceProject | undefined;
    /**
     * Every project `project` reaches through its references, transitively,
     * depth-first in declared order, without itself. Only projects this graph
     * reached from the named config are known, which is every one a project
     * the graph holds can reach.
     */
    closureOf(project: ServiceProject): ServiceProject[];
    private closure;
    /** Position in search order, 0 for the named config. */
    rank(project: ServiceProject): number;
    private projects;
}
/**
 * For the loader: the config each file of the service is typed under. The
 * returned function gives the owner's config path for a file, and the named
 * config for no file; a reference that cannot be read goes to `report`. A named config that references nothing is not parsed
 * here (`references` is never inherited through `extends`), so the function
 * answers the named path for every file and the common case costs nothing.
 */
export declare function serviceConfigPath(configPath: string, report?: (diagnostic: string) => void): (file?: string) => string;
/**
 * Whether two projects' options would emit a declaration the same way, so a
 * type one of them names can be emitted by the other.
 */
export declare function emitsAlike(a: ServiceProject, b: ServiceProject): boolean;
