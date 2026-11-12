/** Deno owns module resolution; both compiler frontends consume its graph. */
import ts from 'typescript';
import { WriteGuard } from './guarded-fs.js';
export interface DenoConfig {
    configPath: string;
    workspaceRoot: string;
    compilerOptions: Record<string, unknown>;
    exclude: string[];
}
/** Explicit TS configs keep their existing behaviour, including mixed repos. */
export declare function findDenoConfig(repoRoot: string, explicit?: string): DenoConfig | undefined;
/** One request's immutable graph, shared by analysis, emit and relocation. */
export declare class DenoProject {
    readonly config: DenoConfig;
    readonly repoRoot: string;
    readonly cacheDir: string;
    readonly parsed: ts.ParsedCommandLine;
    readonly globals: string[];
    readonly diagnostics: string[];
    readonly pinned: Record<string, string>;
    private readonly modules;
    private readonly localPaths;
    private readonly specifiersByPath;
    private readonly edges;
    private readonly redirects;
    private readonly npmPackages;
    private readonly externalNames;
    constructor(config: DenoConfig, repoRoot: string);
    private targetPath;
    resolve(spec: string, from: string, options: ts.CompilerOptions, host?: ts.ModuleResolutionHost): ts.ResolvedModule | undefined;
    private npmOwner;
    /** Which registry package a resolved file belongs to (carrick#1260).
     *
     * The inverse of resolution, and the only answer available for a Deno
     * project: nothing it resolves sits under `node_modules`, so reading a
     * package name off the path cannot work. An npm dependency is owned by the
     * graph entry whose cache directory contains the file; a JSR one is named by
     * its own specifier, since its cached copy is content-addressed and carries
     * no identity. A file the workspace itself owns belongs to no package and
     * answers nothing, exactly as a workspace-declared type does under Node. */
    packageOf(file: string): string | undefined;
    /** The npm package directory a resolved file lies in, and the package's
     * name: its cache directory, which carries no `node_modules` segment, so
     * only the graph can say where the package starts (carrick#2599). */
    packageDirectoryOf(file: string): {
        directory: string;
        name: string;
    } | undefined;
    private isNpmFile;
    /** Let TypeScript interpret real package exports/types using Deno's exact
     * dependency graph. Only the resolution host sees virtual node_modules;
     * every returned source path points at the existing Deno cache. */
    private resolveNpm;
    host(options: ts.CompilerOptions): ts.CompilerHost;
    resolveTypeReference(spec: string, from: string, options: ts.CompilerOptions, host?: ts.ModuleResolutionHost): ts.ResolvedTypeReferenceDirective | undefined;
    /** Turn a Deno npm alias into its real package export, with the exact pin. */
    private externalName;
    /** Relocate Deno's per-file resolutions into the portable declaration tree. */
    rewrite(guard: WriteGuard, typesDir: string, files: string[], sourceByEmitted: Map<string, string>): number;
}
