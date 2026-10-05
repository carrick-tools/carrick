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
 * A package is read from its source whole or not at all. Its declarations
 * travel in the stub with their imports as written, so a file that names a
 * module the checkout does not have (a client that is generated, a dependency
 * that was not installed) would make every type that reaches the file
 * unreadable there, including types that ask nothing of the package. Such a
 * package is left as the compiler found it, which is what it was before any
 * of this.
 *
 * The stub pins one version of a name: the service's, the one its own
 * declarations were read at. A carried declaration that read a dependency at
 * another version keeps its place in the stub, and that one import is left
 * unresolved in that one file (`readsApart`): a member the dependency types
 * reads `unknown` there, and every other type of the package stands. A file
 * that exports such a dependency onward, or extends a type of it, has no
 * position where `unknown` can be written, so a package that reaches one is
 * left as the compiler found it too.
 *
 * Nothing is guessed. A package no project of which writes the entry stays
 * unresolved, and so does an output two of its projects write from different
 * sources; `unbuiltPackageNote` says which, for the reason a reader is given.
 *
 * Seam: node builtins, `typescript`, and this bundle only.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import { isBuiltin } from 'node:module';
import * as path from 'node:path';
import { installOf } from './installed-package.js';
import { checkoutRootOf } from './machinery.js';
import { useBeyondRepair } from './repair-dangling.js';
import { realPath } from './service-config.js';
import { packageNameOf } from './specifiers.js';

/** What a resolution needs beside the compiler's own arguments. */
export interface WorkspaceScope {
  /**
   * The checkout the service sits in. A package is the checkout's own when
   * its real directory is inside this one and inside no `node_modules`.
   */
  root: string;
  /** The service the capture is of: what it installs is what the stub pins. */
  service: string;
}

/**
 * The checkout a service's packages are read from: the scanned root when the
 * caller names one, else the nearest directory above the service that holds a
 * `.git` entry.
 */
export function workspaceScopeOf(serviceRoot: string, scanRoot?: string): WorkspaceScope {
  return {
    root: scanRoot === undefined ? checkoutRootOf(serviceRoot) : path.resolve(scanRoot),
    service: path.resolve(serviceRoot),
  };
}

/**
 * The specifier an anchor's source names, where it names no file
 * (carrick#1175). The scanner hands on a specifier it found no file for
 * joined onto the scanned root. For a service that is that root the join
 * comes off again before the anchor arrives; for a service below it the path
 * arrives whole and names nothing, and its part below the root is the
 * specifier as the source wrote it.
 */
export function sourceSpecifier(source: string, scope: WorkspaceScope): string {
  if (!path.isAbsolute(source) || fs.existsSync(source)) return source;
  const below = path.relative(scope.root, source);
  if (below === '' || below.startsWith('..') || path.isAbsolute(below)) return source;
  return below.split(path.sep).join('/');
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
  /** Its source cannot stand in for its build: `because` is the clause that says why. */
  | { kind: 'incomplete'; because: string }
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
  const entry = entrySource(name, containingFile, options, host, scope, redirected, mode);
  if (entry?.kind !== 'source') return entry;
  const because = incompleteFrom(entry.file, options, host, scope);
  return because === undefined ? entry : { kind: 'incomplete', because };
}

/** The source file a specifier's entry is written from, whatever that source goes on to import. */
function entrySource(
  name: string,
  containingFile: string,
  options: ts.CompilerOptions,
  host: ts.ModuleResolutionHost,
  scope: WorkspaceScope,
  redirected?: ts.ResolvedProjectReference,
  mode?: ts.ResolutionMode
): Exclude<Followed, { kind: 'incomplete' }> {
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

/** What one set of compiler options has found of the checkout's source so far. */
interface Readings {
  /** Source file -> why what it reaches cannot be read whole; undefined when it can. */
  incomplete: Map<string, string | undefined>;
  resolutions: ts.ModuleResolutionCache;
  /**
   * Whether a program under these options holds the Node runtime's types. No
   * file is found for a module of the runtime (`node:events`): its types
   * package declares the module by name, so it resolves in a program that
   * includes that package and in no other.
   */
  runtimeTypes: () => boolean;
}

const readingsByOptions = new WeakMap<ts.CompilerOptions, Map<string, Readings>>();

function readingsFor(options: ts.CompilerOptions, scope: WorkspaceScope): Readings {
  let byScope = readingsByOptions.get(options);
  if (!byScope) readingsByOptions.set(options, (byScope = new Map()));
  const key = `${scope.root}\0${scope.service}`;
  let readings = byScope.get(key);
  if (!readings) {
    let runtimeTypes: boolean | undefined;
    readings = {
      incomplete: new Map(),
      resolutions: ts.createModuleResolutionCache(
        ts.sys.getCurrentDirectory(),
        (file) => (ts.sys.useCaseSensitiveFileNames ? file : file.toLowerCase()),
        options
      ),
      runtimeTypes: () => (runtimeTypes ??= ts.getAutomaticTypeDirectiveNames(options, ts.sys).includes('node')),
    };
    byScope.set(key, readings);
  }
  return readings;
}

/**
 * Why the source a package is entered through cannot stand in for its build,
 * as the clause a reason ends on; undefined when it can.
 *
 * Every source file of the checkout the entry reaches is read, in this
 * package or in another one entered the same way, nearest first. Each module
 * a file names has to resolve under the options the capture's own program
 * resolves with. A dependency read at another version than the service's has
 * to be used only where the stub can write `unknown` for it. A module only an
 * ambient declaration names (a stylesheet, an image) does not resolve here:
 * which declarations the program will hold is not known until it is built.
 *
 * `unresolvedSpecifiersReachableFrom` asks a program that exists what it did
 * not resolve; this is asked while a program's modules are being resolved,
 * and decides what that program will hold.
 */
function incompleteFrom(
  entry: string,
  options: ts.CompilerOptions,
  host: ts.ModuleResolutionHost,
  scope: WorkspaceScope
): string | undefined {
  const readings = readingsFor(options, scope);
  if (readings.incomplete.has(entry)) return readings.incomplete.get(entry);
  const reached = [entry];
  const seen = new Set(reached);
  let because: string | undefined;
  for (let next = 0; next < reached.length && because === undefined; next++) {
    const file = reached[next];
    if (readings.incomplete.has(file)) {
      // Read before, from another entry: whole, or not for the reason found then.
      because = readings.incomplete.get(file);
      continue;
    }
    const text = host.readFile(file);
    if (text === undefined) continue;
    const mode = ts.getImpliedNodeFormatForFile(file, readings.resolutions.getPackageJsonInfoCache(), host, options);
    for (const { fileName: specifier } of ts.preProcessFile(text, true).importedFiles) {
      const resolved =
        ts.resolveModuleName(specifier, file, options, host, readings.resolutions, undefined, mode).resolvedModule
          ?.resolvedFileName ?? sourceOf(entrySource(specifier, file, options, host, scope, undefined, mode));
      if (resolved === undefined) {
        if (isBuiltin(specifier) && readings.runtimeTypes()) continue;
        because = `its source imports '${specifier}', which does not resolve on this checkout either`;
        break;
      }
      if (isOwnSource(resolved, scope)) {
        if (!seen.has(resolved)) reached.push(resolved);
        seen.add(resolved);
        continue;
      }
      const apart = readApart(resolved, scope);
      const use = apart && useBeyondRepair(file, text, specifier);
      if (apart && use) {
        because = `its source ${use} '${apart.name}', which it reads at ${apart.read}, where the service reads it at ${apart.pinned}`;
        break;
      }
    }
  }
  if (because === undefined) for (const file of reached) readings.incomplete.set(file, undefined);
  else readings.incomplete.set(entry, because);
  return because;
}

/**
 * Whether a file an anchor names as its module may join the capture. A source
 * file outside the service is carried into the stub with everything it
 * reaches, so it is held to the rule a package read by name is: a file of the
 * checkout whole or not at all, and a source file of anything else never. A
 * file of the service itself is what it is, and a declaration file is an
 * installed package's, named by its specifier and pinned.
 */
export function carriesWhole(
  file: string,
  options: ts.CompilerOptions,
  host: ts.ModuleResolutionHost,
  scope: WorkspaceScope
): boolean {
  const real = realPath(file);
  if (isInside(realPath(scope.service), real)) return true;
  if (!SOURCE_FILE.test(file) || DECLARATION_FILE.test(file)) return true;
  return isOwnPath(real, scope) && incompleteFrom(file, options, host, scope) === undefined;
}

function sourceOf(followed: Followed): string | undefined {
  return followed?.kind === 'source' ? followed.file : undefined;
}

/** What an installed package is, where a file of it was found. */
type Install = NonNullable<ReturnType<typeof installOf>>;

/** Directory of a resolved file -> the installed package that holds it. */
const installByDirectory = new Map<string, Install | undefined>();

function installHolding(file: string): Install | undefined {
  const dir = path.dirname(file);
  if (!installByDirectory.has(dir)) installByDirectory.set(dir, installOf(file));
  return installByDirectory.get(dir);
}

/**
 * The install the service itself reads a package from: the nearest one above
 * the service's own manifest. Its version is the one the stub pins.
 */
export function serviceInstallOf(name: string, scope: WorkspaceScope): Install | undefined {
  const found = packageInstall(name, path.join(scope.service, 'package.json'));
  return found && installOf(path.join(found.real, 'package.json'));
}

/**
 * A dependency a file of the checkout's source resolved at another version
 * than the service reads it at: its name and the two versions. Undefined
 * where the service installs none, or the same one.
 */
function readApart(
  resolved: string,
  scope: WorkspaceScope
): { name: string; read: string; pinned: string } | undefined {
  const mine = installHolding(resolved);
  if (mine?.version === undefined) return undefined;
  const theirs = serviceInstallOf(mine.name, scope);
  if (theirs?.version === undefined || theirs.version === mine.version) return undefined;
  return { name: mine.name, read: mine.version, pinned: theirs.version };
}

/** One package a carried declaration names, as its source resolved it. */
export interface CarriedRead {
  /** The declaration, by its place in the stub's tree. */
  file: string;
  /** The specifier as the declaration writes it. */
  specifier: string;
  /** The package's name, where it is installed, and its version when that is a published one. */
  install: Install;
}

/**
 * The install each package the carried declarations name is pinned from: the
 * service's own where it has one, else the first a carried declaration read.
 */
export function carriedInstalls(reads: readonly CarriedRead[], scope: WorkspaceScope): Map<string, Install> {
  const installs = new Map<string, Install>();
  for (const { install } of reads) {
    const pinned = installs.get(install.name);
    if (pinned === undefined) installs.set(install.name, serviceInstallOf(install.name, scope) ?? install);
    // A service install that states no version leaves the pin to a read that does.
    else if (pinned.version === undefined && install.version !== undefined) installs.set(install.name, install);
  }
  return installs;
}

/**
 * The imports the stub leaves unresolved because its pin is another version
 * than the declaration read: declaration (by its place in the tree) ->
 * specifier -> the sentence that says so. A type taken from one version of a
 * package is never stated against another.
 */
export function readsApart(
  reads: readonly CarriedRead[],
  pinned: Readonly<Record<string, string>>
): Map<string, Map<string, string>> {
  const apart = new Map<string, Map<string, string>>();
  for (const { file, specifier, install } of reads) {
    const pin = pinned[install.name];
    if (install.version === undefined || pin === undefined || pin === install.version) continue;
    let ofFile = apart.get(file);
    if (!ofFile) apart.set(file, (ofFile = new Map()));
    ofFile.set(
      specifier,
      `this declaration was read with '${install.name}' at ${install.version}, and the stub pins ${pin}, ` +
        'the version the service reads; its import of that package is left unresolved here'
    );
  }
  return apart;
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
 * link is not an external library, where what it reaches can be read whole.
 */
function asOwnSource(
  resolved: ts.ResolvedModuleFull,
  options: ts.CompilerOptions,
  host: ts.ModuleResolutionHost,
  scope: WorkspaceScope
): ts.ResolvedModuleFull {
  if (!resolved.isExternalLibraryImport || !isOwnSource(resolved.resolvedFileName, scope)) return resolved;
  if (incompleteFrom(resolved.resolvedFileName, options, host, scope) !== undefined) return resolved;
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
  if (standard) return asOwnSource(standard, options, host, scope);
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
  // The program reads this cache when it names a module in a declaration it
  // emits (what a package's manifest exports, which names reach it). A host
  // that resolves for itself and does not hand its cache over leaves the
  // program without one, and a type from a package the service does not
  // import by name is then named through another package, or not at all.
  host.getModuleResolutionCache = () => cache;
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
  if (followed?.kind === 'incomplete') {
    return `'${name}' is a package of this checkout whose entry is not on disk, and ${followed.because}`;
  }
  if (followed?.kind === 'ambiguous') {
    return (
      `'${name}' is a package of this checkout whose entry is not on disk, ` +
      'and two tsconfigs in the package write that entry from different source files'
    );
  }
  return undefined;
}
