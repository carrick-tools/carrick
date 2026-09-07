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

import { Project, type CompilerOptions } from 'ts-morph';
import * as path from 'node:path';
import * as fs from 'node:fs';
import type { TsconfigSnapshot, PinnedDependencySnapshot } from './types.js';

/**
 * Options for ProjectLoader construction
 */
export interface ProjectLoaderOptions {
  /** The repository root directory (absolute path) */
  repoRoot: string;
  /** Optional path to tsconfig.json (relative to repo root or absolute) */
  tsconfigPath?: string;
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
  /** Set by `load()`; builds the ts-morph project on first `getProject()`. */
  private buildProject: (() => Project) | null = null;
  private readonly repoRoot: string;
  private readonly tsconfigPath: string | undefined;
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
        this.buildProject = () => {
          const project = new Project({
            compilerOptions: this.snapshotToCompilerOptions(snapshot),
            skipAddingFilesFromTsConfig: true,
          });
          this.addDefaultSourceFiles(project);
          return project;
        };
      }
      // Priority 2: Use tsconfig.json file
      else {
        const tsconfigPath = this.findTsConfig();

        if (tsconfigPath) {
          this.log(`Project will load with tsconfig: ${tsconfigPath}`);
          this.buildProject = () =>
            new Project({
              tsConfigFilePath: tsconfigPath,
              skipAddingFilesFromTsConfig: false,
            });
        } else {
          this.log('No tsconfig.json found, using default compiler options');
          this.buildProject = () => {
            const project = new Project({
              compilerOptions: DEFAULT_COMPILER_OPTIONS,
              skipAddingFilesFromTsConfig: true,
            });
            // Add source files from common locations
            this.addDefaultSourceFiles(project);
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
      this.project = this.buildProject();
      const buildTimeMs = Math.round(performance.now() - startTime);
      this.log(
        `Project built in ${buildTimeMs}ms ` +
          `(${this.project.getSourceFiles().length} source files)`
      );
    }
    return this.project;
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
   * Find the tsconfig.json file to use
   *
   * @returns Absolute path to tsconfig.json, or undefined if not found
   */
  private findTsConfig(): string | undefined {
    // If a specific path was provided, try to use it
    if (this.tsconfigPath) {
      if (fs.existsSync(this.tsconfigPath)) {
        return this.tsconfigPath;
      }
      this.log(`Specified tsconfig not found: ${this.tsconfigPath}`);
    }

    // Try common tsconfig locations
    const candidates = [
      path.join(this.repoRoot, 'tsconfig.json'),
      path.join(this.repoRoot, 'tsconfig.build.json'),
      path.join(this.repoRoot, 'tsconfig.app.json'),
    ];

    for (const candidate of candidates) {
      if (fs.existsSync(candidate)) {
        return candidate;
      }
    }

    return undefined;
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
  private addDefaultSourceFiles(project: Project): void {
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
