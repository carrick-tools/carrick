/**
 * Map an absolute path into an installed package onto the specifier a
 * consumer of that package would write (carrick#1174).
 *
 * The v1 printer and the node builder name an out-of-scope type by the
 * absolute path of its declaration file when no bare specifier reaches it
 * from where they print. Inside a capture stub that path is both private (the
 * checkout root, or the home directory of a runtime's npm cache) and dead:
 * the check workspace resolves packages from the stub's pinned dependencies,
 * never from the scanning machine's disk.
 *
 * The replacement is decided against the package itself, installed alone at
 * `node_modules/<name>`, which is exactly what the check workspace sees:
 *  1. an `exports` entry (or, without `exports`, the types entry or the file's
 *     subpath) that resolves to the same file under bundler resolution;
 *  2. failing that, an exported entry whose module exports the imported name
 *     from that same file (a type in an unexported file, re-exported by the
 *     package root);
 *  3. failing both, the subpath as written, so the self-check reports the
 *     specifier as unresolvable rather than the stub leaking the path.
 *
 * Seam: node builtins, `typescript`, and this bundle only.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { isPublishedSemver } from './lockfile.js';

export interface InstalledPackageSpecifier {
  /** Bare specifier: public package name plus subpath, e.g. `pkg/sub`. */
  specifier: string;
  /** The package's own name and real install directory (never written out). */
  install: { name: string; root: string };
  /** The package the path landed in and its installed version, when published. */
  pin?: { name: string; version: string };
}

interface PackageManifest {
  name: string;
  version: string;
  exports?: unknown;
  types?: string;
  typings?: string;
  main?: string;
}

const DECLARATION_SUFFIXES = ['', '.d.ts', '.d.mts', '.d.cts', '.ts', '.mts', '.cts', '/index.d.ts'];

const RESOLUTION_OPTIONS: ts.CompilerOptions = {
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  target: ts.ScriptTarget.ESNext,
  noEmit: true,
  skipLibCheck: true,
  types: [],
};

/** DefinitelyTyped packages are imported by the runtime name (TS6137). */
function publicNameOf(name: string): string {
  if (!name.startsWith('@types/')) return name;
  const bare = name.slice('@types/'.length);
  const scoped = /^([^_]+)__(.+)$/.exec(bare);
  return scoped ? `@${scoped[1]}/${scoped[2]}` : bare;
}

/**
 * The `@types/*` package that serves a bare name's declarations, for callers
 * deciding whether a runtime name is covered by a pinned types package.
 */
export function typesPackageOf(name: string): string {
  const scoped = /^@([^/]+)\/(.+)$/.exec(name);
  return scoped ? `@types/${scoped[1]}__${scoped[2]}` : `@types/${name}`;
}

function toPosix(p: string): string {
  return p.split(path.sep).join('/');
}

function readManifest(dir: string): PackageManifest | undefined {
  try {
    const parsed = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8'));
    if (typeof parsed?.name === 'string' && typeof parsed?.version === 'string') {
      return parsed as PackageManifest;
    }
  } catch {
    // Absent or unreadable: not a package root.
  }
  return undefined;
}

/**
 * An installed package root is a directory named by its package, either at
 * `node_modules/<name>` (flat and isolated stores alike) or at
 * `<name>/<version>` (a runtime's npm cache). A nested `package.json` that only
 * sets a module type, or a workspace member, is neither.
 */
function isInstalledRoot(dir: string, manifest: PackageManifest): boolean {
  const posix = toPosix(dir);
  return (
    posix.endsWith(`/node_modules/${manifest.name}`) ||
    posix.endsWith(`/${manifest.name}/${manifest.version}`)
  );
}

function packageRootOf(file: string): { root: string; manifest: PackageManifest } | undefined {
  let dir = path.dirname(file);
  while (true) {
    const manifest = readManifest(dir);
    if (manifest && isInstalledRoot(dir, manifest)) return { root: dir, manifest };
    const parent = path.dirname(dir);
    if (parent === dir) return undefined;
    dir = parent;
  }
}

function existingFile(spec: string): string | undefined {
  for (const suffix of DECLARATION_SUFFIXES) {
    const candidate = spec + suffix;
    try {
      if (fs.statSync(candidate).isFile()) return fs.realpathSync(candidate);
    } catch {
      // try the next suffix
    }
  }
  return undefined;
}

function withoutExtension(subpath: string): string {
  return subpath.replace(/(?:\.d)?\.(?:ts|mts|cts|js|mjs|cjs|tsx|jsx)$/, '');
}

/** Every string target an exports value names, whatever its conditions. */
function exportTargets(value: unknown): string[] {
  if (typeof value === 'string') return [value];
  if (Array.isArray(value)) return value.flatMap(exportTargets);
  if (value && typeof value === 'object') return Object.values(value).flatMap(exportTargets);
  return [];
}

/** Candidate subpaths (`''` or `/sub`) that could name `subpath` of the package. */
function candidateSubpaths(manifest: PackageManifest, subpath: string): string[] {
  const target = withoutExtension(subpath);
  const candidates: string[] = [];
  const exportsField = manifest.exports;
  if (exportsField !== undefined && exportsField !== null) {
    const entries: Array<[string, unknown]> =
      typeof exportsField === 'object' &&
      !Array.isArray(exportsField) &&
      Object.keys(exportsField).some((key) => key.startsWith('.'))
        ? Object.entries(exportsField)
        : [['.', exportsField]];
    for (const [key, value] of entries) {
      if (!key.startsWith('.')) continue;
      for (const raw of exportTargets(value)) {
        const exported = withoutExtension(raw.replace(/^\.\//, ''));
        if (!key.includes('*')) {
          if (exported === target) candidates.push(key === '.' ? '' : key.slice(1));
          continue;
        }
        const [before, after] = exported.split('*');
        if (after === undefined || !target.startsWith(before) || !target.endsWith(after)) continue;
        const star = target.slice(before.length, target.length - after.length);
        candidates.push(key.slice(1).replace('*', star));
      }
    }
    return [...new Set(candidates)];
  }
  const entry = manifest.types ?? manifest.typings ?? manifest.main ?? 'index';
  if (withoutExtension(entry.replace(/^\.\//, '')) === target) candidates.push('');
  candidates.push(`/${target.replace(/\/index$/, '')}`);
  return [...new Set(candidates)];
}

/**
 * Resolve `specifier` as the check workspace would: the package installed
 * alone at `node_modules/<name>` beside the importing file. Returns the real
 * path of the file it lands on.
 */
function resolveAgainstInstall(
  specifier: string,
  root: string,
  packageName: string
): ts.ResolvedModuleFull | undefined {
  const virtualRoot = path.join(path.parse(root).root, '__carrick_specifier_probe__');
  const virtualPackage = path.join(virtualRoot, 'node_modules', ...packageName.split('/'));
  const actual = (file: string): string | undefined => {
    if (file === virtualPackage || file.startsWith(virtualPackage + path.sep)) {
      return path.join(root, file.slice(virtualPackage.length));
    }
    return undefined;
  };
  const isVirtualAncestor = (dir: string): boolean =>
    dir === virtualRoot || (virtualPackage + path.sep).startsWith(dir + path.sep);
  const host: ts.ModuleResolutionHost = {
    fileExists: (file) => {
      const real = actual(file);
      return real !== undefined && ts.sys.fileExists(real);
    },
    readFile: (file) => {
      const real = actual(file);
      return real === undefined ? undefined : ts.sys.readFile(real);
    },
    directoryExists: (dir) => {
      if (isVirtualAncestor(dir)) return true;
      const real = actual(dir);
      return real !== undefined && ts.sys.directoryExists(real);
    },
    realpath: (file) => actual(file) ?? file,
    getCurrentDirectory: () => virtualRoot,
  };
  const resolved = ts.resolveModuleName(
    specifier,
    path.join(virtualRoot, 'probe.ts'),
    RESOLUTION_OPTIONS,
    host
  ).resolvedModule;
  if (!resolved) return undefined;
  try {
    const resolvedFileName = fs.realpathSync(actual(resolved.resolvedFileName) ?? resolved.resolvedFileName);
    return { ...resolved, resolvedFileName, isExternalLibraryImport: true };
  } catch {
    return undefined;
  }
}

/**
 * A compiler host that also resolves the packages an absolute specifier was
 * rewritten into, each from its own install directory. The capture's
 * self-check compiles the stub against the producer's `node_modules`, which
 * does not name a transitive package at its root; the check workspace does,
 * because the stub pins it. Without this the self-check would fail a
 * specifier the check resolves.
 */
export function withInstalledPackages(
  options: ts.CompilerOptions,
  installs: Record<string, string>,
  base: ts.CompilerHost = ts.createCompilerHost(options)
): ts.CompilerHost {
  if (Object.keys(installs).length === 0) return base;
  const fallback = base.resolveModuleNames?.bind(base);
  base.resolveModuleNames = (names, containingFile, reused, redirected, compilerOptions, containingSourceFile) =>
    names.map((name, index) => {
      const resolved = fallback
        ? fallback([name], containingFile, reused?.slice(index, index + 1), redirected, compilerOptions, containingSourceFile)[0]
        : ts.resolveModuleName(name, containingFile, compilerOptions, base).resolvedModule;
      if (resolved || name.startsWith('.') || name.startsWith('/')) return resolved;
      const packageName = packageNameOfSpecifier(name);
      for (const owner of [packageName, typesPackageOf(packageName)]) {
        const root = installs[owner];
        if (root) {
          const fromInstall = resolveAgainstInstall(name, root, owner);
          if (fromInstall) return fromInstall;
        }
      }
      return undefined;
    });
  return base;
}

function packageNameOfSpecifier(spec: string): string {
  const parts = spec.split('/');
  return spec.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0];
}

/** Entry subpaths of a package that a consumer can import. */
function entrySubpaths(manifest: PackageManifest): string[] {
  const exportsField = manifest.exports;
  if (exportsField === undefined || exportsField === null) return [''];
  if (
    typeof exportsField === 'object' &&
    !Array.isArray(exportsField) &&
    Object.keys(exportsField).some((key) => key.startsWith('.'))
  ) {
    return Object.keys(exportsField)
      .filter((key) => key.startsWith('.') && !key.includes('*'))
      .map((key) => (key === '.' ? '' : key.slice(1)));
  }
  return [''];
}

const moduleExportsCache = new Map<string, Map<string, Set<string>>>();

/** Exported names of `entryFile`, each with the real files declaring it. */
function exportsOf(entryFile: string): Map<string, Set<string>> {
  const cached = moduleExportsCache.get(entryFile);
  if (cached) return cached;
  const exported = new Map<string, Set<string>>();
  const program = ts.createProgram([entryFile], RESOLUTION_OPTIONS);
  const checker = program.getTypeChecker();
  const source = program.getSourceFile(entryFile);
  const moduleSymbol = source && checker.getSymbolAtLocation(source);
  for (const symbol of moduleSymbol ? checker.getExportsOfModule(moduleSymbol) : []) {
    const target = symbol.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(symbol) : symbol;
    const files = new Set<string>();
    for (const declaration of target.declarations ?? []) {
      try {
        files.add(fs.realpathSync(declaration.getSourceFile().fileName));
      } catch {
        // declaration outside the real file system: ignore
      }
    }
    exported.set(symbol.getName(), files);
  }
  moduleExportsCache.set(entryFile, exported);
  return exported;
}

/**
 * The bare specifier for an absolute path into an installed package, or
 * undefined when the path does not land in one. `importedName` is the first
 * name read off the import (`import("...").Name`), when the text has one.
 */
export function installedPackageSpecifier(
  absoluteSpecifier: string,
  importedName?: string
): InstalledPackageSpecifier | undefined {
  const file = existingFile(absoluteSpecifier);
  if (!file) return undefined;
  const owner = packageRootOf(file);
  if (!owner) return undefined;
  const root = fs.realpathSync(owner.root);
  const { manifest } = owner;
  const publicName = publicNameOf(manifest.name);
  const pin = isPublishedSemver(manifest.version)
    ? { name: manifest.name, version: manifest.version }
    : undefined;
  const subpath = toPosix(path.relative(root, file));
  const install = { name: manifest.name, root };
  const withPin = (specifier: string): InstalledPackageSpecifier =>
    pin ? { specifier, install, pin } : { specifier, install };

  const candidates = candidateSubpaths(manifest, subpath);
  for (const candidate of candidates) {
    if (resolveAgainstInstall(publicName + candidate, root, manifest.name)?.resolvedFileName === file) {
      return withPin(publicName + candidate);
    }
  }

  if (importedName) {
    for (const entry of entrySubpaths(manifest)) {
      const entryFile = resolveAgainstInstall(publicName + entry, root, manifest.name)?.resolvedFileName;
      if (!entryFile) continue;
      if (exportsOf(entryFile).get(importedName)?.has(file)) {
        return withPin(publicName + entry);
      }
    }
  }

  return withPin(publicName + (candidates[0] ?? `/${withoutExtension(subpath)}`));
}
