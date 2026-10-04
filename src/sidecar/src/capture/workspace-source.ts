/**
 * A package of the scanned checkout, read from its source (carrick#1910).
 *
 * In a monorepo a package's manifest usually points its `types` (or an
 * `exports` entry) at build output: `dist/index.d.ts`. On a checkout that was
 * installed and not built that file does not exist, the import does not
 * resolve, and every type the service takes from the package is the
 * compiler's unresolved placeholder, which prints its name and reads `any`.
 *
 * An editor does not need the build: for a project reference it reads the
 * referenced project's config for where each source file's output goes, and
 * opens the source in place of the missing output. This module does the same
 * for any package of the checkout the service imports by name, whether or not
 * the service's config lists it under `references`:
 *
 *  1. The compiler resolves the specifier. When that succeeds nothing here
 *     changes the answer, except to say that a file of the checkout's own
 *     source is not an external library: the resolver marks whatever it
 *     reached through `node_modules` as one, a workspace package is linked
 *     there, and a file marked that way is left out of the declaration emit.
 *  2. When it fails for a bare specifier, the package is looked up the way
 *     Node does. If it is one of the checkout's own packages, the TypeScript
 *     projects in its root say which source file each output is written from
 *     (`ts.getOutputFileNames`, the compiler's own answer under each config's
 *     `outDir`, `rootDir` and `declarationDir`). The compiler resolves the
 *     specifier again against a host in which those outputs exist, so
 *     `exports`, `types`, `typesVersions`, `main` and the import's mode are
 *     read as before, and the output it lands on is answered with its source.
 *
 * Nothing is guessed. A package no project of which writes the entry stays
 * unresolved, and so does an output two of its projects write from different
 * sources; `unbuiltPackageNote` says which, for the reason a reader is given.
 *
 * Seam: node builtins, `typescript`, and this bundle only.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { checkoutRootOf } from './machinery.js';
import { realPath } from './service-config.js';
import { packageNameOf } from './specifiers.js';

/** What a resolution needs beside the compiler's own arguments. */
export interface WorkspaceScope {
  /**
   * The checkout the service sits in. A package is the checkout's own when
   * its real directory is inside this one and inside no `node_modules`.
   */
  root: string;
}

/**
 * The checkout a service's packages are read from: the scanned root when the
 * caller names one, else the nearest directory above the service that holds a
 * `.git` entry.
 */
export function workspaceScopeOf(serviceRoot: string, scanRoot?: string): WorkspaceScope {
  return { root: scanRoot === undefined ? checkoutRootOf(serviceRoot) : path.resolve(scanRoot) };
}

/** The source files a package's TypeScript projects write its outputs from. */
interface PackageOutputs {
  /** Output file -> its source; `null` when two projects name different ones. */
  sources: Map<string, string | null>;
  /** Every directory an output sits in, and each directory above it. */
  directories: Set<string>;
}

const outputsByPackage = new Map<string, PackageOutputs>();

/** Importing directory and package name -> where that package is installed. */
const installedByImporter = new Map<string, { link: string; real: string } | undefined>();

const TSCONFIG_NAME = /^tsconfig(\..+)?\.json$/;
const SOURCE_FILE = /\.[cm]?tsx?$/;
const DECLARATION_FILE = /\.d\.[cm]?ts$/;

/** `a.js` -> `a.d.ts`, `a.mjs` -> `a.d.mts`: the declaration of a script output. */
function declarationBeside(output: string): string | undefined {
  const script = /\.([cm]?)jsx?$/.exec(output);
  return script ? `${output.slice(0, script.index)}.d.${script[1]}ts` : undefined;
}

function isInside(dir: string, file: string): boolean {
  const rel = path.relative(dir, file);
  return rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel));
}

/**
 * Whether a real path is the checkout's own: inside it and inside no
 * `node_modules`. The same test the dependency pinning uses to leave a
 * workspace member unpinned (`installedVersions`).
 */
function isOwnPath(real: string, scope: WorkspaceScope): boolean {
  const rel = path.relative(realPath(scope.root), real);
  if (rel.startsWith('..') || path.isAbsolute(rel)) return false;
  return !rel.split(path.sep).includes('node_modules');
}

/**
 * The directory `specifier`'s package is installed at for `containingFile`:
 * the nearest `node_modules/<name>` above it that holds a manifest, as the
 * link sits on disk, with the directory it leads to.
 */
function packageInstall(
  specifier: string,
  containingFile: string
): { link: string; real: string } | undefined {
  const name = packageNameOf(specifier);
  const segments = name.split('/');
  if (segments.some((segment) => segment === '' || segment === '.' || segment === '..')) {
    return undefined;
  }
  const from = path.dirname(path.resolve(containingFile));
  const key = `${from}\0${name}`;
  if (installedByImporter.has(key)) return installedByImporter.get(key);
  let found: { link: string; real: string } | undefined;
  for (let dir = from; ; ) {
    if (path.basename(dir) !== 'node_modules') {
      const link = path.join(dir, 'node_modules', ...segments);
      if (fs.existsSync(path.join(link, 'package.json'))) {
        found = { link, real: realPath(link) };
        break;
      }
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  installedByImporter.set(key, found);
  return found;
}

/**
 * Which source file each output of a package is written from, by the
 * TypeScript projects in the package's root: every `tsconfig*.json` there, and
 * the projects inside the package those reference. Read once per package.
 */
function packageOutputs(packageDir: string): PackageOutputs {
  const cached = outputsByPackage.get(packageDir);
  if (cached) return cached;
  const sources = new Map<string, string | null>();
  const record = (output: string, source: string): void => {
    const known = sources.get(output);
    if (known === undefined) sources.set(output, source);
    else if (known !== source) sources.set(output, null);
  };
  const visited = new Set<string>();
  const visit = (configPath: string): void => {
    const config = path.resolve(configPath);
    if (visited.has(config)) return;
    visited.add(config);
    let parsed: ts.ParsedCommandLine | undefined;
    try {
      parsed = ts.getParsedCommandLineOfConfigFile(config, {}, {
        ...ts.sys,
        onUnRecoverableConfigFileDiagnostic: () => {},
      });
    } catch {
      return;
    }
    if (!parsed) return;
    for (const fileName of parsed.fileNames) {
      if (!SOURCE_FILE.test(fileName) || DECLARATION_FILE.test(fileName)) continue;
      let outputs: readonly string[];
      try {
        outputs = ts.getOutputFileNames(parsed, fileName, !ts.sys.useCaseSensitiveFileNames);
      } catch {
        continue;
      }
      const source = path.resolve(fileName);
      for (const output of outputs) {
        const named = path.resolve(output);
        if (DECLARATION_FILE.test(named)) record(named, source);
        // The declaration of a script the project writes sits beside it,
        // whichever tool wrote it.
        const beside = declarationBeside(named);
        if (beside) record(beside, source);
      }
    }
    for (const reference of parsed.projectReferences ?? []) {
      const referenced = ts.resolveProjectReferencePath(reference);
      if (isInside(packageDir, referenced)) visit(referenced);
    }
  };
  let names: string[] = [];
  try {
    names = fs.readdirSync(packageDir).filter((name) => TSCONFIG_NAME.test(name)).sort();
  } catch {
    // An unreadable directory has no projects.
  }
  for (const name of names) visit(path.join(packageDir, name));
  const directories = new Set<string>();
  for (const output of sources.keys()) {
    for (let dir = path.dirname(output); isInside(packageDir, dir); dir = path.dirname(dir)) {
      if (directories.has(dir)) break;
      directories.add(dir);
      if (dir === packageDir) break;
    }
  }
  const outputs = { sources, directories };
  outputsByPackage.set(packageDir, outputs);
  return outputs;
}

/** What following an unbuilt package to its source found. */
type Followed =
  | { kind: 'source'; file: string }
  /** The entry is an output two of the package's projects write differently. */
  | { kind: 'ambiguous' }
  /** One of the checkout's packages, and no project of it writes the entry. */
  | { kind: 'unmapped' }
  /** Not one of the checkout's own packages, or not installed. */
  | undefined;

function follow(
  name: string,
  containingFile: string,
  options: ts.CompilerOptions,
  host: ts.ModuleResolutionHost,
  scope: WorkspaceScope,
  redirected?: ts.ResolvedProjectReference,
  mode?: ts.ResolutionMode
): Followed {
  if (name.startsWith('.') || path.isAbsolute(name)) return undefined;
  const installed = packageInstall(name, containingFile);
  if (!installed || !isOwnPath(installed.real, scope)) return undefined;
  const outputs = packageOutputs(installed.real);
  if (outputs.sources.size === 0) return { kind: 'unmapped' };

  // The package as the importing file sees it (through its link) and as its
  // projects name it (its real directory).
  const real = (file: string): string =>
    isInside(installed.link, file)
      ? path.join(installed.real, path.relative(installed.link, file))
      : file;
  const virtual: ts.ModuleResolutionHost = {
    ...host,
    fileExists: (file) => host.fileExists(file) || outputs.sources.has(real(file)),
    directoryExists: (dir) =>
      (host.directoryExists?.(dir) ?? ts.sys.directoryExists(dir)) || outputs.directories.has(real(dir)),
    realpath: (file) =>
      outputs.sources.has(real(file)) ? real(file) : (host.realpath?.(file) ?? realPath(file)),
  };
  const resolved = ts.resolveModuleName(name, containingFile, options, virtual, undefined, redirected, mode)
    .resolvedModule;
  if (!resolved) return { kind: 'unmapped' };
  const source = outputs.sources.get(real(resolved.resolvedFileName));
  if (source === undefined) return { kind: 'unmapped' };
  return source === null ? { kind: 'ambiguous' } : { kind: 'source', file: source };
}

function extensionOf(file: string): ts.Extension {
  const found = [ts.Extension.Tsx, ts.Extension.Mts, ts.Extension.Cts, ts.Extension.Ts].find((extension) =>
    file.endsWith(extension)
  );
  return found ?? ts.Extension.Ts;
}

/**
 * Whether a file is source the checkout owns: a TypeScript source file (not a
 * declaration) inside the checkout and inside no `node_modules`. The
 * declaration emit writes such a file's declaration into the stub wherever in
 * the checkout it sits.
 */
export function isOwnSource(file: string, scope: WorkspaceScope): boolean {
  return SOURCE_FILE.test(file) && !DECLARATION_FILE.test(file) && isOwnPath(realPath(file), scope);
}

/**
 * The compiler's answer with the checkout's own source read as source: a
 * `.ts` file of the checkout the resolver reached through a `node_modules`
 * link is not an external library.
 */
function asOwnSource(resolved: ts.ResolvedModuleFull, scope: WorkspaceScope): ts.ResolvedModuleFull {
  if (!resolved.isExternalLibraryImport || !isOwnSource(resolved.resolvedFileName, scope)) return resolved;
  return { ...resolved, isExternalLibraryImport: false };
}

/**
 * The source file an unbuilt package of the checkout is entered through, for
 * a specifier the compiler did not resolve; undefined when there is none.
 */
export function unbuiltPackageSource(
  name: string,
  containingFile: string,
  options: ts.CompilerOptions,
  host: ts.ModuleResolutionHost,
  scope: WorkspaceScope,
  redirected?: ts.ResolvedProjectReference,
  mode?: ts.ResolutionMode
): ts.ResolvedModuleFull | undefined {
  const followed = follow(name, containingFile, options, host, scope, redirected, mode);
  if (followed?.kind !== 'source') return undefined;
  return {
    resolvedFileName: followed.file,
    extension: extensionOf(followed.file),
    isExternalLibraryImport: false,
  };
}

/** Resolve a module as the compiler does, then as the header describes. */
export function resolveModule(
  name: string,
  containingFile: string,
  options: ts.CompilerOptions,
  host: ts.ModuleResolutionHost,
  scope: WorkspaceScope,
  cache?: ts.ModuleResolutionCache,
  redirected?: ts.ResolvedProjectReference,
  mode?: ts.ResolutionMode
): ts.ResolvedModuleFull | undefined {
  const standard = ts.resolveModuleName(name, containingFile, options, host, cache, redirected, mode)
    .resolvedModule;
  if (standard) return asOwnSource(standard, scope);
  return unbuiltPackageSource(name, containingFile, options, host, scope, redirected, mode);
}

/**
 * A compiler host whose programs read the checkout's packages from source.
 * Each import resolves in the mode the compiler reads off it.
 */
export function workspaceCompilerHost(options: ts.CompilerOptions, scope: WorkspaceScope): ts.CompilerHost {
  const host = ts.createCompilerHost(options);
  const cache = ts.createModuleResolutionCache(
    host.getCurrentDirectory(),
    (file) => host.getCanonicalFileName(file),
    options
  );
  host.resolveModuleNameLiterals = (literals, containingFile, redirected, compilerOptions, containingSourceFile) =>
    literals.map((literal) => ({
      resolvedModule: resolveModule(
        literal.text,
        containingFile,
        compilerOptions,
        host,
        scope,
        cache,
        redirected,
        ts.getModeForUsageLocation(containingSourceFile, literal, compilerOptions)
      ),
    }));
  return host;
}

/**
 * Why a bare specifier that did not resolve is one a build would resolve: a
 * sentence when it names one of the checkout's own packages, undefined for
 * anything else (a dependency that is not installed, a path).
 */
export function unbuiltPackageNote(
  name: string,
  containingFile: string,
  options: ts.CompilerOptions,
  scope: WorkspaceScope
): string | undefined {
  const followed = follow(name, containingFile, options, ts.sys, scope);
  if (followed?.kind === 'unmapped') {
    return (
      `'${name}' is a package of this checkout whose entry is not on disk, ` +
      'and no tsconfig in the package writes that entry from a source file'
    );
  }
  if (followed?.kind === 'ambiguous') {
    return (
      `'${name}' is a package of this checkout whose entry is not on disk, ` +
      'and two tsconfigs in the package write that entry from different source files'
    );
  }
  return undefined;
}
