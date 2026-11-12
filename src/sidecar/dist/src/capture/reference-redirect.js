/**
 * How a program built from one owner config reads the projects that config
 * references (carrick#2418): as an editor reads them.
 *
 * A workspace package a service references is usually not built on a scanned
 * checkout: its `package.json` points `types` at `dist/index.d.ts`, which
 * does not exist, so the import read as the error type. The compiler's editor
 * mode (`projectReferences` with `useSourceOfProjectReferenceRedirect`) reads
 * the package's source instead, under the package's own options. The
 * sidecar's programs cannot be handed references (ts-morph never passes them,
 * and the capture's emit would then skip the sources), so this module does
 * what the compiler's `updateHostForUseSourceOfProjectReferenceRedirect` does,
 * in the resolver:
 *
 *  1. Every project in the owner's reference closure is parsed through the
 *     read bound (`ProjectGraph`), its options through
 *     `sidecarCompilerOptions`.
 *  2. Each of its inputs maps to its declaration output, by the compiler's
 *     own `getOutputFileNames` with declarations on (a project with `outFile`
 *     has no per-file map and is skipped). A project that leaves `rootDir`
 *     unset is laid out as TypeScript 5 lays it out: under the config's
 *     directory when `composite`, else under the common directory of its
 *     inputs. TypeScript 6 puts every such project under the config's
 *     directory (`src/index.ts` to `dist/src/index.d.ts`), where a package
 *     written against 5 points its `types` at `dist/index.d.ts`.
 *  3. The resolver's host answers that an output exists while its source
 *     does, and that the directories outputs sit in exist. An output is
 *     looked up by the real path of its deepest existing ancestor: a package
 *     reached through a `node_modules` link has no `dist/`, so the real path
 *     of the output itself is the link path and would miss the map.
 *  4. An import made from a referenced project's own file resolves under that
 *     project's options (`redirectedReference`), and an answer that lands on
 *     an output becomes its source. A source inside the checkout and under no
 *     `node_modules` is not an external-library import, so the origin gates
 *     read it as the user's code.
 *
 * A package the owner does not reference stays unresolved, as in the editor.
 * Nothing here writes, and every read goes through the host it wraps.
 */
import * as fs from 'node:fs';
import * as path from 'node:path';
import ts from 'typescript';
import { boundedSys, checkoutRootOf, isInsideCheckout, realOf, untrustedReadBound } from './checkout.js';
import { sidecarCompilerOptions } from './compiler-options.js';
import { ProjectGraph } from './project-references.js';
const DECLARATION = /\.d\.[cm]?ts$/;
/** `realOf`, cached per directory: the resolver asks about many files in few directories. */
function realPaths() {
    const dirs = new Map();
    return (p) => {
        const abs = path.resolve(p);
        const dir = path.dirname(abs);
        let real = dirs.get(dir);
        if (real === undefined) {
            real = realOf(dir);
            dirs.set(dir, real);
        }
        return path.join(real, path.basename(abs));
    };
}
/** The directory every input sits under, as TypeScript computes it. */
function commonDirectory(files, ignoreCase) {
    let common;
    const key = (part) => (ignoreCase ? part.toLowerCase() : part);
    for (const file of files) {
        const parts = path.dirname(path.resolve(file)).split(path.sep);
        if (!common) {
            common = parts;
            continue;
        }
        let n = 0;
        while (n < common.length && n < parts.length && key(common[n]) === key(parts[n]))
            n += 1;
        common = common.slice(0, n);
    }
    return common && common.length > 0 ? common.join(path.sep) || path.sep : undefined;
}
/**
 * `parsed` with declarations on and, when it leaves `rootDir` unset,
 * TypeScript 5's root (see the header, step 2).
 */
function declarationLayout(parsed, ignoreCase) {
    const options = { ...parsed.options, declaration: true, emitDeclarationOnly: true };
    if (!options.rootDir) {
        const rootDir = options.composite
            ? path.dirname(path.resolve(String(options.configFilePath)))
            : commonDirectory(parsed.fileNames.filter((file) => !DECLARATION.test(file) && !file.endsWith('.json')), ignoreCase);
        if (rootDir)
            options.rootDir = rootDir;
    }
    return { ...parsed, options };
}
/**
 * The redirect for a program whose owner reaches `references` (its reference
 * closure, from `ProjectGraph.closureOf`), or `undefined` when there are none.
 * An output two of the projects write from different sources goes to the
 * first one, or, with `ambiguousUnresolved`, to neither: nothing says which
 * the entry is, so the import stays unresolved (carrick#1910).
 */
export function referenceRedirect(references, repoRoot, { ambiguousUnresolved = false } = {}) {
    if (references.length === 0)
        return undefined;
    const realOf = realPaths();
    const ignoreCase = !ts.sys.useCaseSensitiveFileNames;
    /** Real path of an output -> its source. */
    const outputs = new Map();
    /** Outputs two projects write from different sources (`ambiguousUnresolved`). */
    const ambiguous = new Set();
    /** Real paths of the directories outputs sit in, up to their project's. */
    const outputDirs = new Set();
    /** Base names of outputs and of their directories: the cheap test first. */
    const names = new Set();
    const byFile = new Map();
    for (const project of references) {
        const configFile = project.parsed.options.configFile;
        if (!configFile)
            continue;
        // Its own imports resolve under its options with TypeScript 5's defaults.
        const commandLine = { ...project.parsed, options: sidecarCompilerOptions(project.parsed.options) };
        const reference = { commandLine, sourceFile: configFile };
        const projectDir = realOf(path.dirname(project.configPath));
        const layout = project.parsed.options.outFile ? undefined : declarationLayout(project.parsed, ignoreCase);
        for (const file of project.parsed.fileNames) {
            const key = path.resolve(file);
            if (!byFile.has(key))
                byFile.set(key, reference);
            if (!layout || file.endsWith('.json') || DECLARATION.test(file))
                continue;
            const declaration = ts.getOutputFileNames(layout, file, ignoreCase).find((out) => DECLARATION.test(out));
            if (!declaration)
                continue;
            const output = realOf(declaration);
            const known = outputs.get(output);
            if (known !== undefined) {
                if (ambiguousUnresolved && known !== key)
                    ambiguous.add(output);
                continue;
            }
            outputs.set(output, key);
            names.add(path.basename(output));
            for (let dir = path.dirname(output); !outputDirs.has(dir); dir = path.dirname(dir)) {
                outputDirs.add(dir);
                names.add(path.basename(dir));
                if (dir === projectDir || path.dirname(dir) === dir)
                    break;
            }
        }
    }
    for (const output of ambiguous)
        outputs.delete(output);
    const sourceOf = (file) => DECLARATION.test(file) && names.has(path.basename(file)) ? outputs.get(realOf(file)) : undefined;
    const isOutputDir = (dir) => names.has(path.basename(dir)) && outputDirs.has(realOf(dir));
    return {
        referenceFor: (containingFile) => byFile.get(path.resolve(containingFile)),
        resolutionHost: (host) => {
            const wrapped = Object.create(host);
            wrapped.fileExists = (file) => {
                if (host.fileExists(file))
                    return true;
                const source = sourceOf(file);
                return source !== undefined && host.fileExists(source);
            };
            if (host.directoryExists) {
                const directoryExists = host.directoryExists.bind(host);
                wrapped.directoryExists = (dir) => directoryExists(dir) || isOutputDir(dir);
            }
            return wrapped;
        },
        sourceAnswer: (answer) => {
            if (!answer)
                return answer;
            const source = sourceOf(answer.resolvedFileName);
            if (!source)
                return answer;
            const own = isInsideCheckout(source.replace(/\\/g, '/'), repoRoot) &&
                !source.includes(`${path.sep}node_modules${path.sep}`);
            return {
                ...answer,
                resolvedFileName: source,
                extension: path.extname(source),
                resolvedUsingTsExtension: false,
                isExternalLibraryImport: own ? false : answer.isExternalLibraryImport,
            };
        },
    };
}
/**
 * For the loader, which builds a program from one config path: the redirect
 * for that config's reference closure. A config that references nothing is
 * not parsed (`references` is never inherited through `extends`).
 */
export function referenceRedirectOf(configPath, repoRoot) {
    const sys = boundedSys(untrustedReadBound(path.dirname(configPath)));
    const raw = ts.readConfigFile(configPath, (file) => sys.readFile(file)).config;
    if (!Array.isArray(raw?.references) || raw.references.length === 0)
        return undefined;
    let graph;
    try {
        graph = new ProjectGraph(configPath);
    }
    catch {
        return undefined;
    }
    return referenceRedirect(graph.closureOf(graph.named), repoRoot);
}
/**
 * The directories of the packages an install links into the checkout, as
 * seen from `fromDir`: every `node_modules/<name>` and
 * `node_modules/@scope/<name>` from `fromDir` up to the checkout root whose
 * real path is inside the checkout, under no `node_modules`, and holds a
 * `package.json`. A link that resolves to `fromDir` or above it is the
 * service itself and is left out.
 */
function linkedWorkspacePackages(fromDir, checkoutRoot) {
    const root = realOf(checkoutRoot);
    const from = realOf(fromDir);
    const found = new Set();
    const consider = (link) => {
        let real;
        try {
            if (!fs.lstatSync(link).isSymbolicLink())
                return;
            real = fs.realpathSync(link);
        }
        catch {
            return;
        }
        const rel = path.relative(root, real);
        if (rel === '' || rel.startsWith('..') || path.isAbsolute(rel))
            return;
        if (rel.split(path.sep).includes('node_modules'))
            return;
        const toFrom = path.relative(real, from);
        if (toFrom === '' || (!toFrom.startsWith('..') && !path.isAbsolute(toFrom)))
            return;
        if (fs.existsSync(path.join(real, 'package.json')))
            found.add(real);
    };
    const entries = (dir) => {
        try {
            return fs.readdirSync(dir, { withFileTypes: true });
        }
        catch {
            return [];
        }
    };
    for (let dir = path.resolve(fromDir);; dir = path.dirname(dir)) {
        const modules = path.join(dir, 'node_modules');
        for (const entry of entries(modules)) {
            if (entry.name.startsWith('.'))
                continue;
            const link = path.join(modules, entry.name);
            if (entry.name.startsWith('@')) {
                for (const scoped of entries(link))
                    consider(path.join(link, scoped.name));
            }
            else {
                consider(link);
            }
        }
        if (realOf(dir) === root || path.dirname(dir) === dir)
            break;
    }
    return [...found].sort();
}
const TSCONFIG_NAME = /^tsconfig(\..+)?\.json$/;
const projectsByPackage = new Map();
/** Every `tsconfig*.json` in `packageDir`, parsed, with the projects each references. */
function packageProjects(packageDir) {
    const cached = projectsByPackage.get(packageDir);
    if (cached)
        return cached;
    const projects = readPackageProjects(packageDir);
    projectsByPackage.set(packageDir, projects);
    return projects;
}
function readPackageProjects(packageDir) {
    let names;
    try {
        names = fs.readdirSync(packageDir).filter((name) => TSCONFIG_NAME.test(name)).sort();
    }
    catch {
        return [];
    }
    const projects = [];
    for (const name of names) {
        try {
            const graph = new ProjectGraph(path.join(packageDir, name));
            projects.push(graph.named, ...graph.closureOf(graph.named));
        }
        catch {
            // A config that cannot be read writes nothing anyone can follow.
        }
    }
    return projects;
}
/**
 * For the position rows' process only (carrick#1910): `referenceRedirectOf`,
 * with the projects of every workspace package the install links into the
 * checkout added to the reference closure, whether or not the config
 * references them. A package's projects are every `tsconfig*.json` in its
 * root. An output two of them write from different sources is followed to
 * neither. Every other program keeps `referenceRedirectOf`: infer's text is
 * the compat judge's anchor, so widening it would move verdicts.
 */
export function workspaceSourceRedirectOf(configPath, repoRoot) {
    let references = [];
    try {
        const graph = new ProjectGraph(configPath);
        references = graph.closureOf(graph.named);
    }
    catch {
        // An unreadable owner config references nothing.
    }
    const seen = new Set([path.resolve(configPath), ...references.map((p) => path.resolve(p.configPath))]);
    const projects = [...references];
    for (const packageDir of linkedWorkspacePackages(path.dirname(configPath), checkoutRootOf(repoRoot))) {
        for (const project of packageProjects(packageDir)) {
            const key = path.resolve(project.configPath);
            if (seen.has(key))
                continue;
            seen.add(key);
            projects.push(project);
        }
    }
    return referenceRedirect(projects, repoRoot, { ambiguousUnresolved: true });
}
/**
 * For the capture's anchor program: `host` resolving every import through
 * `redirect`, with the module resolution cache it uses handed to the program.
 * A host with its own resolver and no cache makes declaration emit name a
 * transitive package through another (TS2742).
 */
export function redirectingCompilerHost(host, options, redirect) {
    const resolutionHost = redirect.resolutionHost(host);
    const cache = ts.createModuleResolutionCache(host.getCurrentDirectory(), (fileName) => host.getCanonicalFileName(fileName), options);
    host.getModuleResolutionCache = () => cache;
    host.resolveModuleNameLiterals = (literals, containingFile, redirectedReference, compilerOptions, containingSourceFile) => literals.map((literal) => {
        const answer = ts.resolveModuleName(literal.text, containingFile, compilerOptions, resolutionHost, cache, redirectedReference ?? redirect.referenceFor(containingFile), ts.getModeForUsageLocation(containingSourceFile, literal, compilerOptions));
        return { ...answer, resolvedModule: redirect.sourceAnswer(answer.resolvedModule) };
    });
    return host;
}
