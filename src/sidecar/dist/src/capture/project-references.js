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
import * as path from 'node:path';
import ts from 'typescript';
import { boundedSys, untrustedReadBound } from './checkout.js';
/** Parse one config; throws on a file that cannot be read or parsed. */
function parseConfig(configPath, extendedConfigCache) {
    const host = {
        ...boundedSys(untrustedReadBound(path.dirname(configPath))),
        onUnRecoverableConfigFileDiagnostic: (d) => {
            throw new Error(ts.flattenDiagnosticMessageText(d.messageText, '\n'));
        },
    };
    const parsed = ts.getParsedCommandLineOfConfigFile(configPath, {}, host, extendedConfigCache);
    if (!parsed)
        throw new Error(`failed to parse ${configPath}`);
    return { configPath: path.resolve(configPath), parsed };
}
const fileKey = (file) => path.resolve(file);
/**
 * The projects a named config reaches and the owner of each file among them.
 * References are read on the first question about a file, and never for a
 * named config that references nothing, which costs one parse, as before.
 */
export class ProjectGraph {
    named;
    /** References that could not be read, one line each. */
    diagnostics = [];
    extendedConfigCache = new Map();
    walked;
    namedFiles;
    closures = new Map();
    /** Throws when the named config cannot be read, as a bare parse would. */
    constructor(configPath) {
        this.named = parseConfig(configPath, this.extendedConfigCache);
        this.namedFiles = new Set(this.named.parsed.fileNames.map(fileKey));
    }
    /** Whether the named config references other projects at all. */
    get hasReferences() {
        return (this.named.parsed.projectReferences?.length ?? 0) > 0;
    }
    /**
     * The project that owns `file`: the first in search order (the named
     * config, then its references depth-first in declared order; a config
     * reached twice is visited once) that lists it while no project in its own
     * reference closure lists it. The named config when none does.
     */
    ownerOf(file) {
        if (!this.hasReferences)
            return this.named;
        return this.listingOwnerOf(file) ?? this.named;
    }
    /**
     * `ownerOf` when some project in the graph lists `file`, the named config
     * included; undefined when none does (carrick#3497: a config that lists a
     * file nowhere is passed over for one further up).
     */
    listingOwnerOf(file) {
        const key = fileKey(file);
        if (!this.hasReferences)
            return this.namedFiles.has(key) ? this.named : undefined;
        const owner = this.projects().find((entry) => entry.files.has(key) && !this.closure(entry.project).some((reached) => reached.files.has(key)));
        return owner?.project;
    }
    /**
     * Every project `project` reaches through its references, transitively,
     * depth-first in declared order, without itself. Only projects this graph
     * reached from the named config are known, which is every one a project
     * the graph holds can reach.
     */
    closureOf(project) {
        return this.hasReferences ? this.closure(project).map((reached) => reached.project) : [];
    }
    closure(project) {
        let found = this.closures.get(project);
        if (found)
            return found;
        const byPath = new Map(this.projects().map((entry) => [entry.project.configPath, entry]));
        const seen = new Set([project.configPath]);
        found = [];
        const visit = (from) => {
            for (const reference of from.parsed.projectReferences ?? []) {
                const referencePath = path.resolve(ts.resolveProjectReferencePath(reference));
                if (seen.has(referencePath))
                    continue;
                seen.add(referencePath);
                const entry = byPath.get(referencePath);
                if (!entry)
                    continue;
                found.push(entry);
                visit(entry.project);
            }
        };
        visit(project);
        this.closures.set(project, found);
        return found;
    }
    /** Position in search order, 0 for the named config. */
    rank(project) {
        if (project === this.named)
            return 0;
        const index = this.projects().findIndex((entry) => entry.project === project);
        return index === -1 ? Number.MAX_SAFE_INTEGER : index;
    }
    projects() {
        if (this.walked)
            return this.walked;
        const visited = new Set([this.named.configPath]);
        const order = [];
        const visit = (project, files) => {
            order.push({ project, files });
            for (const reference of project.parsed.projectReferences ?? []) {
                const referencePath = path.resolve(ts.resolveProjectReferencePath(reference));
                if (visited.has(referencePath))
                    continue;
                visited.add(referencePath);
                let child;
                try {
                    child = parseConfig(referencePath, this.extendedConfigCache);
                }
                catch (err) {
                    this.diagnostics.push(`referenced tsconfig ${referencePath} (from ${project.configPath}) could not be read and was skipped: ` +
                        (err instanceof Error ? err.message : String(err)));
                    continue;
                }
                visit(child, new Set(child.parsed.fileNames.map(fileKey)));
            }
        };
        visit(this.named, this.namedFiles);
        this.walked = order;
        return order;
    }
}
/**
 * For the loader: the config each file of the service is typed under. The
 * returned function gives the owner's config path for a file, and the named
 * config for no file; a reference that cannot be read goes to `report`. A named config that references nothing is not parsed
 * here (`references` is never inherited through `extends`), so the function
 * answers the named path for every file and the common case costs nothing.
 */
export function serviceConfigPath(configPath, report = () => { }) {
    const raw = ts.readConfigFile(configPath, ts.sys.readFile).config;
    if (!Array.isArray(raw?.references) || raw.references.length === 0) {
        return () => configPath;
    }
    let graph;
    return (file) => {
        if (file === undefined)
            return configPath;
        graph ??= new ProjectGraph(configPath);
        const reported = graph.diagnostics.length;
        const owner = graph.ownerOf(file);
        graph.diagnostics.slice(reported).forEach(report);
        return owner === graph.named ? configPath : owner.configPath;
    };
}
/** Compiler options that only place output, so two projects differing only in these emit the same declarations. */
const OUTPUT_ONLY_OPTIONS = new Set([
    'rootDir',
    'outDir',
    'declarationDir',
    'outFile',
    'composite',
    'declaration',
    'declarationMap',
    'emitDeclarationOnly',
    'incremental',
    'tsBuildInfoFile',
    'configFilePath',
    'noEmit',
    'sourceMap',
    'inlineSourceMap',
    'inlineSources',
]);
/**
 * Whether two projects' options would emit a declaration the same way, so a
 * type one of them names can be emitted by the other.
 */
export function emitsAlike(a, b) {
    const relevant = (options) => JSON.stringify(Object.keys(options)
        .filter((key) => !OUTPUT_ONLY_OPTIONS.has(key))
        .sort()
        .map((key) => [key, options[key]]));
    return relevant(a.parsed.options) === relevant(b.parsed.options);
}
