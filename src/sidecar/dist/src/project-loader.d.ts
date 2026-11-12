/**
 * Project Loader - Loads TypeScript projects using ts-morph
 *
 * This module handles initialization of the ts-morph Project,
 * including finding and loading tsconfig.json files.
 *
 * Supports:
 * - Traditional tsconfig.json file loading
 * - Tsconfig snapshot (closed/merged extends chains) for synthetic monorepo
 * - Pinned dependency snapshots for deterministic builds
 */
import { Project } from 'ts-morph';
import type { TsconfigSnapshot, PinnedDependencySnapshot } from './types.js';
/**
 * Options for ProjectLoader construction
 */
export interface ProjectLoaderOptions {
    /** The repository root directory (absolute path) */
    repoRoot: string;
    /** Optional path to tsconfig.json (relative to repo root or absolute) */
    tsconfigPath?: string;
    /**
     * The scanned repo's root, the upper bound of the search for a tsconfig
     * above `repoRoot` when none is named (carrick#1776). Without it only
     * `repoRoot` is searched.
     */
    scanRoot?: string;
    /** Optional tsconfig snapshot (closed/merged) - preferred over tsconfigPath */
    tsconfigSnapshot?: TsconfigSnapshot;
    /** Optional pinned dependencies for this repo */
    pinnedDependencies?: PinnedDependencySnapshot;
    /**
     * The position rows' process only (carrick#1910): every program also reads
     * the workspace packages the install links into the checkout from their
     * source (`workspaceSourceRedirectOf`). Off for every other process.
     */
    workspaceSource?: boolean;
}
/**
 * Result of project loading
 */
export interface LoadResult {
    success: boolean;
    error?: string;
    initTimeMs?: number;
}
/**
 * What a build makes (carrick#2413). `listed` is the project as its config
 * lists it. `scripts` has the same options with JavaScript accepted and lists
 * no file: it is for the files the listed program does not take (a `.js`,
 * `.mjs` or `.cjs` file under a config without `allowJs`), which the caller
 * adds.
 */
export type BuildKind = 'listed' | 'scripts';
/**
 * ProjectLoader - Manages ts-morph Project initialization and access
 *
 * `load()` decides what the project will be built from and returns; the
 * ts-morph project itself is built on first use and memoised. Readiness is a
 * gate on the whole type layer — when it was gated on program construction, a
 * repo with its dependencies installed could walk past the caller's budget and
 * lose types for every service at once (carrick#749). Building on demand puts
 * that cost inside the request that needs it, where a failure costs one
 * service instead of all of them.
 *
 * Usage:
 *   const loader = new ProjectLoader({ repoRoot: '/path/to/repo' });
 *   const result = loader.load();
 *   if (result.success) {
 *     const project = loader.getProject();
 *   }
 */
export declare class ProjectLoader {
    private project;
    /**
     * Set by `load()`; builds the ts-morph project on first `getProject()`,
     * and the default project's `scripts` program for `scriptProjectFor`.
     */
    private buildProject;
    /**
     * The Deno graph this project resolved through, when it did (carrick#1260).
     * It is the only thing that can name the package a resolved file belongs to,
     * because a Deno project resolves nothing under `node_modules`. Set by the
     * build, so it is there from the first `getProject()` onwards.
     */
    private denoProject;
    /** The tsconfig the service names, when the project is built from one. */
    private namedTsconfigPath;
    /**
     * The config each file is typed under (carrick#1604, carrick#3497): set for
     * a service built from a tsconfig, with it as the fallback, and for one
     * with no config at or above its root, with none. Unset (a snapshot or a
     * Deno project), every file is the default project's.
     */
    private governing;
    /** Programs built for owning projects other than the named tsconfig. */
    private readonly ownerProjects;
    private readonly repoRoot;
    private readonly tsconfigPath;
    private readonly scanRoot;
    /** `ProjectLoaderOptions.workspaceSource`. */
    private readonly workspaceSource;
    private readonly tsconfigSnapshot;
    private readonly pinnedDependencies;
    private initialized;
    private initError;
    private initTimeMs;
    constructor(options: ProjectLoaderOptions);
    /**
     * Decide what the project will be built from.
     *
     * Validates the repo root and resolves the tsconfig, both cheap; the
     * ts-morph project is built by the first `getProject()`.
     *
     * @returns LoadResult indicating success or failure
     */
    load(): LoadResult;
    /**
     * Convert a TsconfigSnapshot to ts-morph CompilerOptions
     */
    private snapshotToCompilerOptions;
    /**
     * The file system every project reads through: the default library from
     * the compiler's own files (carrick#3509) and, under CARRICK_UNTRUSTED_TREE,
     * nothing whose real path is outside the checkout (carrick#2230).
     */
    private fileSystemOption;
    /**
     * A ts-morph project built from one tsconfig: with the files it lists, or
     * as its `scripts` program (`BuildKind`).
     */
    private projectFromConfig;
    /**
     * Which project types `file` (carrick#1604, carrick#3497): the key of its
     * owning project's program, or `''` for the default one. The owner is the
     * config `GoverningConfigs` picks: the nearest config inside the service
     * whose program, or a project it references (carrick#2418's search order),
     * lists the file, else the service's tsconfig. The service's own tsconfig,
     * every request that names no file, and a file no config governs use the
     * default project.
     */
    projectKeyFor(file?: string): string;
    /**
     * The project behind a key from `projectKeyFor`, built on first use and
     * kept: a service whose files all belong to one project builds one.
     */
    getProjectFor(key: string): Project;
    /**
     * The `scripts` program (`BuildKind`) of the project behind a key from
     * `projectKeyFor`: its options with JavaScript accepted, built by the same
     * factory, and no file listed. Not kept; the caller drops it. For the walk's
     * files that the owner's program does not take (carrick#2413).
     */
    scriptProjectFor(key: string): Project;
    /**
     * Drop the program behind a key from `projectKeyFor`, so the memory it
     * holds can go before the next one is built. The next `getProjectFor` on
     * the key builds it again. For a walk that reads one owner's program at a
     * time and never comes back to it (carrick#2405); the actions that keep
     * their programs for the life of the process never call it.
     */
    releaseProjectFor(key: string): void;
    /**
     * Get the ts-morph Project, building it on first call.
     *
     * @returns The Project instance
     * @throws Error if load() has not succeeded, or if the build fails
     */
    getProject(): Project;
    /**
     * Whether the project behind a key from `projectKeyFor` has been built,
     * without building it (carrick#2843).
     */
    isBuilt(key: string): boolean;
    /**
     * Whether the project resolves through a module graph read outside the
     * compiler (Deno, carrick#1260). What such a program reads is not all in
     * its source files (carrick#2843).
     */
    resolvesThroughModuleGraph(): boolean;
    /**
     * The default project when a request has built it, and `undefined` before
     * that, without building it (carrick#2027).
     */
    builtProject(): Project | undefined;
    /**
     * Every program a request has built: the default project and each owning
     * project's (carrick#1604), without building any (carrick#3100).
     */
    builtProjects(): Project[];
    /** The scanned repo's root when init named one, else the service root. */
    getScanRoot(): string;
    /**
     * The registry package a resolved file belongs to, when the module graph
     * names one (carrick#1260). `undefined` for a project that did not resolve
     * through Deno, and for a file the workspace itself owns.
     */
    packageOf(filePath: string): string | undefined;
    /**
     * The installed package directory a resolved file lies in, and its name,
     * when the module graph places it (carrick#2599). `undefined` for a project
     * that did not resolve through Deno: there the path says it.
     */
    packageDirectoryOf(filePath: string): {
        directory: string;
        name: string;
    } | undefined;
    /**
     * How the module graph resolves `name` from `from`, as the program did
     * (carrick#2599). `undefined` for a project that did not resolve through
     * Deno: there Node resolution is the program's.
     */
    resolveThroughGraph(name: string, from: string, options: import('typescript').CompilerOptions): import('typescript').ResolvedModuleFull | undefined;
    /**
     * Check if the project has been successfully initialized
     */
    isInitialized(): boolean;
    /**
     * Get initialization time in milliseconds
     */
    getInitTimeMs(): number | null;
    /**
     * Get the last initialization error, if any
     */
    getInitError(): string | null;
    /**
     * Get the repository root path
     */
    getRepoRoot(): string;
    /**
     * Get the pinned dependencies, if any
     */
    getPinnedDependencies(): PinnedDependencySnapshot | undefined;
    /**
     * The tsconfig to build from: the named one when it exists, else the one
     * `findServiceTsconfig` finds for the service, which capture also reads.
     *
     * @returns Absolute path to the tsconfig, or undefined if not found
     */
    private findTsConfig;
    /**
     * Add source files from common project locations when no tsconfig is found.
     *
     * Files are globbed and added one by one rather than through
     * `addSourceFilesAtPaths`, which registers every descendant directory of any
     * directory a pattern hit in. One `.ts` file at the repo root is enough to
     * make that walk the entire installed tree: on a large monorepo with
     * dependencies installed it cost 40 s and added nothing the compiler needs
     * (carrick#749).
     */
    private addDefaultSourceFiles;
    /**
     * Log a message to stderr (stdout is reserved for JSON responses)
     */
    private log;
    /**
     * Log an error message to stderr
     */
    private logError;
}
