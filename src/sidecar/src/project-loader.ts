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

import { Project, getCompilerOptionsFromTsConfig, type CompilerOptions, type FileSystemHost } from 'ts-morph';
import * as path from 'node:path';
import ts from 'typescript';
import * as fs from 'node:fs';
import type { TsconfigSnapshot, PinnedDependencySnapshot } from './types.js';
import {
  DenoProject,
  findDenoConfig,
  fileConfigPath,
  findServiceTsconfig,
  referenceRedirectOf,
  serviceConfigPath,
  sidecarCompilerOptions,
  untrustedReadBound,
  type ReadBound,
} from './capture/index.js';
import { moduleFormatResolutionHost } from './module-format.js';
import { ExternalImports, registerExternalImports } from './origin.js';

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
 * Source-file patterns used when the repo declares no tsconfig, relative to
 * the repo root. `node_modules` is excluded explicitly: a glob that matches
 * anything at the repo root would otherwise pull the whole installed tree in.
 */
const DEFAULT_SOURCE_PATTERNS = [
  'src/**/*.ts',
  'src/**/*.tsx',
  'lib/**/*.ts',
  'app/**/*.ts',
  'app/**/*.tsx',
  '*.ts',
];

/**
 * Default compiler options used when no tsconfig.json is found
 */
const DEFAULT_COMPILER_OPTIONS: CompilerOptions = {
  target: 99, // ESNext
  module: 99, // ESNext
  moduleResolution: 99, // NodeNext
  strict: true,
  esModuleInterop: true,
  skipLibCheck: true,
  declaration: true,
  allowJs: true,
  checkJs: false,
  resolveJsonModule: true,
  isolatedModules: true,
};

/**
 * What a build makes (carrick#2413). `listed` is the project as its config
 * lists it. `scripts` has the same options with JavaScript accepted and lists
 * no file: it is for the files the listed program does not take (a `.js`,
 * `.mjs` or `.cjs` file under a config without `allowJs`), which the caller
 * adds.
 */
export type BuildKind = 'listed' | 'scripts';

/** `options` as `kind` builds them. */
function optionsFor<O extends CompilerOptions>(kind: BuildKind, options: O): O {
  return kind === 'scripts' ? { ...options, allowJs: true } : options;
}

/**
 * Map string module values to ts-morph enum values
 */
const MODULE_MAP: Record<string, number> = {
  'CommonJS': 1,
  'AMD': 2,
  'UMD': 3,
  'System': 4,
  'ES2015': 5,
  'ES2020': 6,
  'ES2022': 7,
  'ESNext': 99,
  'Node16': 100,
  'NodeNext': 199,
  'Preserve': 200,
};

/**
 * Map string moduleResolution values to ts-morph enum values
 */
const MODULE_RESOLUTION_MAP: Record<string, number> = {
  'Classic': 1,
  'Node': 2,
  'Node10': 2,
  'Node16': 3,
  'NodeNext': 99,
  'Bundler': 100,
};

/**
 * Map string target values to ts-morph enum values
 */
const TARGET_MAP: Record<string, number> = {
  'ES3': 0,
  'ES5': 1,
  'ES2015': 2,
  'ES2016': 3,
  'ES2017': 4,
  'ES2018': 5,
  'ES2019': 6,
  'ES2020': 7,
  'ES2021': 8,
  'ES2022': 9,
  'ES2023': 10,
  'ESNext': 99,
};

/** Whether the config's own `compilerOptions` state `maxNodeModuleJsDepth`. */
function statesJsDepth(configPath: string): boolean {
  const raw = ts.readConfigFile(configPath, ts.sys.readFile).config as
    | { compilerOptions?: Record<string, unknown> }
    | undefined;
  return raw?.compilerOptions?.maxNodeModuleJsDepth !== undefined;
}

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
export class ProjectLoader {
  private project: Project | null = null;
  /**
   * Set by `load()`; builds the ts-morph project on first `getProject()`,
   * and the default project's `scripts` program for `scriptProjectFor`.
   */
  private buildProject: ((kind: BuildKind) => Project) | null = null;
  /**
   * The Deno graph this project resolved through, when it did (carrick#1260).
   * It is the only thing that can name the package a resolved file belongs to,
   * because a Deno project resolves nothing under `node_modules`. Set by the
   * build, so it is there from the first `getProject()` onwards.
   */
  private denoProject: DenoProject | undefined;
  /** The tsconfig the service names, when the project is built from one. */
  private namedTsconfigPath: string | undefined;
  /**
   * Set when the service has no config at or above its root: each file is then
   * typed under the nearest `tsconfig.json`/`jsconfig.json` on its way up to
   * the service root (carrick#1604), and the default project holds the rest.
   */
  private inferredService = false;
  /** The per-directory answers of `fileConfigPath` for this service. */
  private readonly fileConfigs = new Map<string, string | undefined>();
  /** The reference owner rule of each config a file was found under. */
  private readonly ownerRules = new Map<string, (file?: string) => string>();
  /** Each file's owning config (carrick#1604), resolved on first use. */
  private configPathFor: ((file?: string) => string) | undefined;
  /** Programs built for owning projects other than the named tsconfig. */
  private readonly ownerProjects = new Map<string, Project>();
  private readonly repoRoot: string;
  private readonly tsconfigPath: string | undefined;
  private readonly scanRoot: string | undefined;
  private readonly tsconfigSnapshot: TsconfigSnapshot | undefined;
  private readonly pinnedDependencies: PinnedDependencySnapshot | undefined;
  private initialized: boolean = false;
  private initError: string | null = null;
  private initTimeMs: number | null = null;

  constructor(options: ProjectLoaderOptions) {
    // Normalize the repo root to an absolute path
    this.repoRoot = path.isAbsolute(options.repoRoot)
      ? options.repoRoot
      : path.resolve(process.cwd(), options.repoRoot);

    // Resolve tsconfig path if provided
    if (options.tsconfigPath) {
      this.tsconfigPath = path.isAbsolute(options.tsconfigPath)
        ? options.tsconfigPath
        : path.resolve(this.repoRoot, options.tsconfigPath);
    }

    this.scanRoot = options.scanRoot;

    // Store snapshot if provided
    this.tsconfigSnapshot = options.tsconfigSnapshot;
    this.pinnedDependencies = options.pinnedDependencies;
  }

  /**
   * Decide what the project will be built from.
   *
   * Validates the repo root and resolves the tsconfig, both cheap; the
   * ts-morph project is built by the first `getProject()`.
   *
   * @returns LoadResult indicating success or failure
   */
  load(): LoadResult {
    const startTime = performance.now();

    try {
      // Validate repo root exists
      if (!fs.existsSync(this.repoRoot)) {
        const error = `Repository root does not exist: ${this.repoRoot}`;
        this.logError(error);
        this.initError = error;
        return { success: false, error };
      }

      if (!fs.statSync(this.repoRoot).isDirectory()) {
        const error = `Repository root is not a directory: ${this.repoRoot}`;
        this.logError(error);
        this.initError = error;
        return { success: false, error };
      }

      // Priority 1: Use tsconfig snapshot if provided (for synthetic monorepo)
      if (this.tsconfigSnapshot) {
        const snapshot = this.tsconfigSnapshot;
        this.log('Project will load with tsconfig snapshot');
        this.buildProject = (kind) => {
          const project = new Project({
            ...this.fileSystemOption(),
            compilerOptions: sidecarCompilerOptions(optionsFor(kind, this.snapshotToCompilerOptions(snapshot))),
            skipAddingFilesFromTsConfig: true,
          });
          if (kind === 'listed') this.addDefaultSourceFiles(project);
          return project;
        };
      }
      // Priority 2: Use tsconfig.json file
      else {
        const tsconfigPath = this.findTsConfig();
        const denoConfig = findDenoConfig(this.repoRoot, this.tsconfigPath);

        if (denoConfig) {
          this.log(`Project will load with Deno config: ${denoConfig.configPath}`);
          this.buildProject = (kind) => {
            const deno = new DenoProject(denoConfig, this.repoRoot);
            this.denoProject = deno;
            // The graph's external-library verdicts, kept past the program
            // rebuilds that drop them (carrick#1731).
            const imports = new ExternalImports();
            const project = new Project({
              ...this.fileSystemOption(),
              compilerOptions: sidecarCompilerOptions(optionsFor(kind, deno.parsed.options as CompilerOptions)),
              skipAddingFilesFromTsConfig: true,
              resolutionHost: imports.recording((host, getOptions) => ({
                resolveModuleNames: (names, from) => names.map(name =>
                  deno.resolve(name, from, getOptions() as import('typescript').CompilerOptions, host)),
                resolveTypeReferenceDirectives: (names, from) => names.map(name =>
                  deno.resolveTypeReference(typeof name === 'string' ? name : name.fileName, from,
                    getOptions() as import('typescript').CompilerOptions, host)),
              })),
            });
            registerExternalImports(project, imports);
            if (kind === 'listed') {
              for (const file of deno.parsed.fileNames) project.addSourceFileAtPath(file);
              for (const diagnostic of deno.diagnostics) this.logError(diagnostic);
            }
            return project;
          };
        } else if (tsconfigPath) {
          this.log(`Project will load with tsconfig: ${tsconfigPath}`);
          this.namedTsconfigPath = tsconfigPath;
          this.buildProject = (kind) => this.projectFromConfig(tsconfigPath, kind);
        } else {
          this.log('No tsconfig.json found, using default compiler options');
          this.inferredService = true;
          this.buildProject = (kind) => {
            const project = new Project({
              ...this.fileSystemOption(),
              compilerOptions: sidecarCompilerOptions(optionsFor(kind, DEFAULT_COMPILER_OPTIONS)),
              skipAddingFilesFromTsConfig: true,
            });
            // Add source files from common locations
            if (kind === 'listed') this.addDefaultSourceFiles(project, (file) => this.projectKeyFor(file) === '');
            return project;
          };
        }
      }

      // Log pinned dependencies if provided
      if (this.pinnedDependencies) {
        const depCount = Object.keys(this.pinnedDependencies).length;
        this.log(`Using ${depCount} pinned dependencies`);
      }

      this.initialized = true;
      this.initTimeMs = Math.round(performance.now() - startTime);
      this.log(`Project resolved in ${this.initTimeMs}ms (built on first use)`);

      return {
        success: true,
        initTimeMs: this.initTimeMs,
      };
    } catch (err) {
      const error = err instanceof Error ? err.message : String(err);
      this.logError(`Failed to load project: ${error}`);
      this.initError = error;

      return {
        success: false,
        error,
        initTimeMs: Math.round(performance.now() - startTime),
      };
    }
  }

  /**
   * Convert a TsconfigSnapshot to ts-morph CompilerOptions
   */
  private snapshotToCompilerOptions(snapshot: TsconfigSnapshot): CompilerOptions {
    const opts = snapshot.compilerOptions;
    const result: CompilerOptions = {};

    // Map module
    if (opts.module) {
      const moduleValue = MODULE_MAP[opts.module];
      if (moduleValue !== undefined) {
        result.module = moduleValue;
      }
    }

    // Map moduleResolution
    if (opts.moduleResolution) {
      const moduleResValue = MODULE_RESOLUTION_MAP[opts.moduleResolution];
      if (moduleResValue !== undefined) {
        result.moduleResolution = moduleResValue;
      }
    }

    // Map target
    if (opts.target) {
      const targetValue = TARGET_MAP[opts.target];
      if (targetValue !== undefined) {
        result.target = targetValue;
      }
    }

    // Pass through other options directly
    if (opts.lib) result.lib = opts.lib;
    if (opts.types) result.types = opts.types;
    if (opts.typeRoots) result.typeRoots = opts.typeRoots;
    if (opts.strict !== undefined) result.strict = opts.strict;
    if (opts.esModuleInterop !== undefined) result.esModuleInterop = opts.esModuleInterop;
    if (opts.skipLibCheck !== undefined) result.skipLibCheck = opts.skipLibCheck;
    if (opts.declaration !== undefined) result.declaration = opts.declaration;
    if (opts.declarationMap !== undefined) result.declarationMap = opts.declarationMap;
    if (opts.paths) result.paths = opts.paths;
    if (opts.baseUrl) result.baseUrl = opts.baseUrl;

    // Map jsx if present
    if (opts.jsx) {
      const jsxMap: Record<string, number> = {
        'preserve': 1,
        'react': 2,
        'react-native': 3,
        'react-jsx': 4,
        'react-jsxdev': 5,
      };
      const jsxValue = jsxMap[opts.jsx.toLowerCase()];
      if (jsxValue !== undefined) {
        result.jsx = jsxValue;
      }
    }

    return result;
  }

  /**
   * Under CARRICK_UNTRUSTED_TREE, the file system every project reads
   * through: nothing whose real path is outside the checkout (carrick#2230).
   * Nothing at all otherwise, so a project is built as it always was.
   */
  private fileSystemOption(): { fileSystem?: FileSystemHost } {
    const bound = untrustedReadBound(this.repoRoot);
    return bound ? { fileSystem: boundedFileSystem(bound) } : {};
  }

  /**
   * A ts-morph project built from one tsconfig: with the files it lists, or
   * as its `scripts` program (`BuildKind`).
   */
  private projectFromConfig(configPath: string, kind: BuildKind = 'listed'): Project {
    const imports = new ExternalImports();
    const options = getCompilerOptionsFromTsConfig(configPath).options;
    // TypeScript reads a `jsconfig.json` with an implicit depth of 2, which
    // loads the JavaScript sources of installed packages; every other program
    // here reads 0. A depth the config states is kept (carrick#1604).
    if (this.inferredService && path.basename(configPath) === 'jsconfig.json' && !statesJsDepth(configPath)) {
      options.maxNodeModuleJsDepth = 0;
    }
    const project = new Project({
      ...this.fileSystemOption(),
      tsConfigFilePath: configPath,
      // Over the tsconfig's own options: ts-morph lays these on top.
      compilerOptions: sidecarCompilerOptions(optionsFor(kind, options)),
      skipAddingFilesFromTsConfig: kind === 'scripts',
      // Each import resolves in its file's own module format (carrick#1619),
      // a referenced project's unbuilt output reads as its source, under its
      // own options (carrick#2418), and every external-library answer is kept
      // past the program rebuilds that drop it (carrick#1731).
      resolutionHost: imports.recording(moduleFormatResolutionHost(referenceRedirectOf(configPath, this.repoRoot))),
    });
    registerExternalImports(project, imports);
    return project;
  }

  /**
   * Which project types `file` (carrick#1604): the key of its owning
   * project's program, or `''` for the default one. In a tsconfig that
   * references other projects, a file belongs to the first project in search
   * order (the named config, then its references depth-first, in declared
   * order) that lists it while none of its own references does (carrick#2418),
   * and is typed under that project's options. A file no project includes, and every request that
   * names no file, uses the default project, built from the named tsconfig
   * as before.
   */
  projectKeyFor(file?: string): string {
    if (this.inferredService) return this.inferredKeyFor(file);
    if (!this.namedTsconfigPath || file === undefined) return '';
    this.configPathFor ??= serviceConfigPath(this.namedTsconfigPath, (d) => this.logError(d));
    const config = this.configPathFor(path.resolve(this.repoRoot, file));
    return config === this.namedTsconfigPath ? '' : config;
  }

  /**
   * `projectKeyFor` for a service with no config at its root: the owner of the
   * nearest config on the file's way up to the service root, or `''` (the
   * default project, under the inferred options) for a file with none.
   */
  private inferredKeyFor(file?: string): string {
    if (file === undefined) return '';
    const absolute = path.resolve(this.repoRoot, file);
    const config = fileConfigPath(absolute, this.repoRoot, this.fileConfigs);
    if (config === undefined) return '';
    let owner = this.ownerRules.get(config);
    if (!owner) {
      owner = serviceConfigPath(config, (d) => this.logError(d));
      this.ownerRules.set(config, owner);
    }
    return owner(absolute);
  }

  /**
   * The project behind a key from `projectKeyFor`, built on first use and
   * kept: a service whose files all belong to one project builds one.
   */
  getProjectFor(key: string): Project {
    if (key === '') return this.getProject();
    let project = this.ownerProjects.get(key);
    if (!project) {
      const startTime = performance.now();
      project = this.projectFromConfig(key);
      this.ownerProjects.set(key, project);
      this.log(
        `Project for ${key}, the project that owns the requested files, built in ` +
          `${Math.round(performance.now() - startTime)}ms (${project.getSourceFiles().length} source files)`
      );
    }
    return project;
  }

  /**
   * The `scripts` program (`BuildKind`) of the project behind a key from
   * `projectKeyFor`: its options with JavaScript accepted, built by the same
   * factory, and no file listed. Not kept; the caller drops it. For the walk's
   * files that the owner's program does not take (carrick#2413).
   */
  scriptProjectFor(key: string): Project {
    if (!this.initialized || !this.buildProject) {
      throw new Error('Project not initialized. Call load() first and ensure it succeeds.');
    }
    return key === '' ? this.buildProject('scripts') : this.projectFromConfig(key, 'scripts');
  }

  /**
   * Drop the program behind a key from `projectKeyFor`, so the memory it
   * holds can go before the next one is built. The next `getProjectFor` on
   * the key builds it again. For a walk that reads one owner's program at a
   * time and never comes back to it (carrick#2405); the actions that keep
   * their programs for the life of the process never call it.
   */
  releaseProjectFor(key: string): void {
    if (key === '') {
      this.project = null;
    } else {
      this.ownerProjects.delete(key);
    }
  }

  /**
   * Get the ts-morph Project, building it on first call.
   *
   * @returns The Project instance
   * @throws Error if load() has not succeeded, or if the build fails
   */
  getProject(): Project {
    if (!this.initialized || !this.buildProject) {
      throw new Error(
        'Project not initialized. Call load() first and ensure it succeeds.'
      );
    }
    if (!this.project) {
      const startTime = performance.now();
      this.project = this.buildProject('listed');
      const buildTimeMs = Math.round(performance.now() - startTime);
      this.log(
        `Project built in ${buildTimeMs}ms ` +
          `(${this.project.getSourceFiles().length} source files)`
      );
    }
    return this.project;
  }

  /**
   * Whether the project behind a key from `projectKeyFor` has been built,
   * without building it (carrick#2843).
   */
  isBuilt(key: string): boolean {
    return key === '' ? this.project !== null : this.ownerProjects.has(key);
  }

  /**
   * Whether the project resolves through a module graph read outside the
   * compiler (Deno, carrick#1260). What such a program reads is not all in
   * its source files (carrick#2843).
   */
  resolvesThroughModuleGraph(): boolean {
    return this.denoProject !== undefined;
  }

  /**
   * The default project when a request has built it, and `undefined` before
   * that, without building it (carrick#2027).
   */
  builtProject(): Project | undefined {
    return this.project ?? undefined;
  }

  /**
   * Every program a request has built: the default project and each owning
   * project's (carrick#1604), without building any (carrick#3100).
   */
  builtProjects(): Project[] {
    return [...(this.project ? [this.project] : []), ...this.ownerProjects.values()];
  }

  /** The scanned repo's root when init named one, else the service root. */
  getScanRoot(): string {
    return this.scanRoot ?? this.repoRoot;
  }

  /**
   * The registry package a resolved file belongs to, when the module graph
   * names one (carrick#1260). `undefined` for a project that did not resolve
   * through Deno, and for a file the workspace itself owns.
   */
  packageOf(filePath: string): string | undefined {
    return this.denoProject?.packageOf(filePath);
  }

  /**
   * The installed package directory a resolved file lies in, and its name,
   * when the module graph places it (carrick#2599). `undefined` for a project
   * that did not resolve through Deno: there the path says it.
   */
  packageDirectoryOf(filePath: string): { directory: string; name: string } | undefined {
    return this.denoProject?.packageDirectoryOf(filePath);
  }

  /**
   * How the module graph resolves `name` from `from`, as the program did
   * (carrick#2599). `undefined` for a project that did not resolve through
   * Deno: there Node resolution is the program's.
   */
  resolveThroughGraph(
    name: string,
    from: string,
    options: import('typescript').CompilerOptions
  ): import('typescript').ResolvedModuleFull | undefined {
    return this.denoProject?.resolve(name, from, options) as import('typescript').ResolvedModuleFull | undefined;
  }

  /**
   * Check if the project has been successfully initialized
   */
  isInitialized(): boolean {
    return this.initialized;
  }

  /**
   * Get initialization time in milliseconds
   */
  getInitTimeMs(): number | null {
    return this.initTimeMs;
  }

  /**
   * Get the last initialization error, if any
   */
  getInitError(): string | null {
    return this.initError;
  }

  /**
   * Get the repository root path
   */
  getRepoRoot(): string {
    return this.repoRoot;
  }

  /**
   * Get the pinned dependencies, if any
   */
  getPinnedDependencies(): PinnedDependencySnapshot | undefined {
    return this.pinnedDependencies;
  }

  /**
   * The tsconfig to build from: the named one when it exists, else the one
   * `findServiceTsconfig` finds for the service, which capture also reads.
   *
   * @returns Absolute path to the tsconfig, or undefined if not found
   */
  private findTsConfig(): string | undefined {
    if (this.tsconfigPath) {
      if (fs.existsSync(this.tsconfigPath)) {
        return this.tsconfigPath;
      }
      this.log(`Specified tsconfig not found: ${this.tsconfigPath}`);
    }
    return findServiceTsconfig(this.repoRoot, this.scanRoot);
  }

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
  private addDefaultSourceFiles(project: Project, keep: (file: string) => boolean = () => true): void {
    const globs = DEFAULT_SOURCE_PATTERNS.map((pattern) =>
      path.join(this.repoRoot, pattern)
    );
    // Negated last so it applies to every pattern above it.
    globs.push(`!${path.join(this.repoRoot, '**/node_modules/**')}`);

    let filePaths: string[] = [];
    try {
      filePaths = project.getFileSystem().globSync(globs);
    } catch (err) {
      this.logError(
        `Default source file glob failed: ${err instanceof Error ? err.message : String(err)}`
      );
      return;
    }

    for (const filePath of filePaths) {
      if (!keep(filePath)) continue;
      try {
        project.addSourceFileAtPath(filePath);
      } catch {
        // A file that disappeared between glob and read is not fatal.
      }
    }

    this.log(
      `Added ${project.getSourceFiles().length} source files from default patterns`
    );
  }

  /**
   * Log a message to stderr (stdout is reserved for JSON responses)
   */
  private log(message: string): void {
    console.error(`[sidecar:project-loader] ${message}`);
  }

  /**
   * Log an error message to stderr
   */
  private logError(message: string): void {
    console.error(`[sidecar:project-loader:error] ${message}`);
  }
}

/**
 * A ts-morph file system that reads only inside the bound. ts-morph reads the
 * tsconfig, lists the files it names and reads each through this host, and its
 * module resolution asks it whether a file exists. Writes are not bounded:
 * the write guard owns them (carrick#1748).
 */
function boundedFileSystem(bound: ReadBound): FileSystemHost {
  const base = new Project({
    compilerOptions: sidecarCompilerOptions({}),
    skipAddingFilesFromTsConfig: true,
    skipFileDependencyResolution: true,
  }).getFileSystem();
  const missing = (filePath: string): Error =>
    Object.assign(new Error(`ENOENT: no such file or directory, open '${filePath}'`), { code: 'ENOENT' });
  return new Proxy(base, {
    get(target, prop, receiver) {
      const value = Reflect.get(target, prop, receiver) as unknown;
      if (typeof value !== 'function') return value;
      const fn = value.bind(target) as (...args: any[]) => any;
      switch (prop) {
        case 'readFileSync':
          return (p: string, e?: string) => {
            if (!bound.allows(p)) throw missing(p);
            return fn(p, e);
          };
        case 'readFile':
          return async (p: string, e?: string) => {
            if (!bound.allows(p)) throw missing(p);
            return fn(p, e);
          };
        case 'fileExistsSync':
        case 'directoryExistsSync':
          return (p: string) => bound.allows(p) && fn(p);
        case 'fileExists':
        case 'directoryExists':
          return async (p: string) => bound.allows(p) && (await fn(p));
        case 'readDirSync':
          return (p: string) => (bound.allows(p) ? fn(p) : []);
        case 'globSync':
          return (patterns: string[]) => (fn(patterns) as string[]).filter((f) => bound.allows(f));
        case 'glob':
          return async (patterns: string[]) => ((await fn(patterns)) as string[]).filter((f) => bound.allows(f));
        default:
          return fn;
      }
    },
  });
}
