/** Deno owns module resolution; both compiler frontends consume its graph. */
import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { rewriteSpecifiers } from './specifiers.js';

interface Config {
  workspace?: string[];
  compilerOptions?: Record<string, unknown>;
  exclude?: string[];
}
interface Resolution { specifier?: string; error?: string }
interface Dependency { specifier: string; code?: Resolution; type?: Resolution }
interface Module {
  specifier: string;
  local?: string;
  mediaType?: string;
  kind?: string;
  error?: string;
  dependencies?: Dependency[];
  typesDependency?: { dependency: Resolution };
}
interface Graph {
  version: number;
  modules: Module[];
  redirects?: Record<string, string>;
}
export interface DenoConfig {
  configPath: string;
  workspaceRoot: string;
  compilerOptions: Record<string, unknown>;
  exclude: string[];
}

function readConfig(file: string): Config {
  const parsed = ts.parseConfigFileTextToJson(file, fs.readFileSync(file, 'utf8'));
  if (parsed.error) throw new Error(ts.flattenDiagnosticMessageText(parsed.error.messageText, '\n'));
  return parsed.config as Config;
}

/** Explicit TS configs keep their existing behaviour, including mixed repos. */
export function findDenoConfig(repoRoot: string, explicit?: string): DenoConfig | undefined {
  if (explicit && !/^deno\.jsonc?$/.test(path.basename(explicit))) return undefined;
  let dir = path.resolve(repoRoot);
  const configs: { file: string; config: Config }[] = [];
  while (true) {
    const file = ['deno.json', 'deno.jsonc'].map(n => path.join(dir, n)).find(f => fs.existsSync(f));
    if (file) configs.push({ file, config: readConfig(file) });
    if (fs.existsSync(path.join(dir, '.git')) || path.dirname(dir) === dir) break;
    dir = path.dirname(dir);
  }
  if (!configs.length) return undefined;
  const nearest = configs[0];
  const declaredWorkspace = configs.find(c => c.config.workspace?.some(member => {
    const memberRoot = path.resolve(path.dirname(c.file), member);
    const relative = path.relative(memberRoot, path.resolve(repoRoot));
    return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative));
  }));
  if (!explicit && path.dirname(nearest.file) !== path.resolve(repoRoot) &&
      fs.existsSync(path.join(repoRoot, 'package.json')) && !declaredWorkspace) return undefined;
  const workspace = declaredWorkspace ?? nearest;
  const opts = { ...workspace.config.compilerOptions, ...nearest.config.compilerOptions };
  const typesOwner = nearest.config.compilerOptions?.types !== undefined ? nearest.file : workspace.file;
  if (Array.isArray(opts.types)) {
    opts.types = opts.types.map((spec: unknown) => typeof spec === 'string' && (spec.startsWith('.') || path.isAbsolute(spec))
      ? pathToFileURL(path.resolve(path.dirname(typesOwner), spec)).href : spec);
  }
  return {
    configPath: explicit ? path.resolve(repoRoot, explicit) : nearest.file,
    workspaceRoot: path.dirname(workspace.file),
    compilerOptions: opts,
    exclude: [...(workspace.config.exclude ?? []).map(p => path.resolve(path.dirname(workspace.file), p)),
      ...(nearest.config.exclude ?? []).map(p => path.resolve(path.dirname(nearest.file), p))],
  };
}

function runDeno(args: string[], cwd: string): string {
  try {
    return execFileSync('deno', args, {
      cwd, encoding: 'utf8', timeout: 120_000, maxBuffer: 64 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, DENO_NO_UPDATE_CHECK: '1' },
    });
  } catch (err) {
    const detail = err as { stderr?: string; message?: string };
    throw new Error(`Deno type preparation failed. Install Deno on PATH and prepare the project's dependencies without lifecycle scripts before scanning. ${detail.stderr || detail.message}`);
  }
}

/** One request's immutable graph, shared by analysis, emit and relocation. */
export class DenoProject {
  readonly cacheDir: string;
  readonly parsed: ts.ParsedCommandLine;
  readonly globals: string[];
  readonly diagnostics: string[] = [];
  readonly pinned: Record<string, string> = {};
  private readonly modules = new Map<string, Module>();
  private readonly localPaths = new Map<string, string>();
  private readonly edges = new Map<string, Map<string, Resolution>>();
  private readonly redirects: Record<string, string>;
  private readonly externalNames = new Map<string, string>();

  constructor(readonly config: DenoConfig, readonly repoRoot: string) {
    const cache = path.join(config.workspaceRoot, '.carrick', 'deno', createHash('sha256').update(path.resolve(repoRoot)).digest('hex').slice(0, 16));
    this.cacheDir = cache;
    fs.mkdirSync(cache, { recursive: true });
    const raw = { ...config.compilerOptions };
    const libs = Array.isArray(raw.lib) ? raw.lib as string[] : ['deno.window'];
    const denoLibs = libs.filter(l => l.startsWith('deno.'));
    if (denoLibs.some(l => !['deno.window', 'deno.ns', 'deno.unstable'].includes(l))) {
      throw new Error(`Unsupported Deno type libraries: ${denoLibs.join(', ')}. Supported runtime scopes are deno.window and deno.ns.`);
    }
    delete raw.types;
    delete raw.jsxImportSourceTypes;
    const runtime = denoLibs.includes('deno.window');
    this.parsed = ts.parseJsonConfigFileContent({
      compilerOptions: {
        target: 'ESNext', module: 'ESNext', moduleResolution: 'Bundler', strict: true,
        allowJs: true, checkJs: false, skipLibCheck: true, resolveJsonModule: true,
        ...raw, lib: ['esnext', ...libs.filter(l => !l.startsWith('deno.'))],
        allowImportingTsExtensions: true, noEmit: true,
      },
      include: ['**/*.ts', '**/*.tsx', '**/*.mts', '**/*.cts', '**/*.js', '**/*.jsx'],
      exclude: ['**/node_modules/**', '**/.git/**', '**/.carrick/**', '**/dist/**', '**/.vite/**', ...config.exclude],
    }, ts.sys, repoRoot);
    this.parsed.options.rootDir = config.workspaceRoot;
    this.globals = [];
    if (denoLibs.length) {
      const declarations = runDeno(['types'], repoRoot);
      // deno types concatenates its libs, leaving references to Deno-only lib
      // names that stock TypeScript cannot load. The declarations themselves
      // come unchanged from the installed compiler; no ambient stand-ins.
      let text = declarations.replace(/^\/\/\/\s*<reference\s+(?:no-default-lib="true"|lib="deno\.[^"]+")\s*\/>\s*$/gm, '');
      if (!runtime) {
        const source = ts.createSourceFile('runtime.d.ts', text, ts.ScriptTarget.Latest, true);
        text = source.statements.filter(s =>
          (ts.isModuleDeclaration(s) && (s.name.text === 'Deno' || denoLibs.includes('deno.unstable'))) ||
          (ts.isInterfaceDeclaration(s) && s.name.text === 'ImportMeta')
        ).map(s => s.getFullText(source)).join('\n');
      }
      const globalPath = path.join(cache, 'runtime.d.ts');
      fs.writeFileSync(globalPath, text);
      this.globals.push(globalPath);
    }
    // The graph root lives within the service so compilerOptions.types uses
    // that member's import-map scope, just like the real source files do.
    const entry = path.join(repoRoot, '.carrick', 'deno', 'graph.ts');
    fs.mkdirSync(path.dirname(entry), { recursive: true });
    const roots = this.parsed.fileNames.map(f => pathToFileURL(f).href);
    const extraTypes = config.compilerOptions.types;
    if (Array.isArray(extraTypes)) roots.push(...extraTypes.filter((t): t is string => typeof t === 'string'));
    fs.writeFileSync(entry, roots.map(s => `import ${JSON.stringify(s)};`).join('\n'));
    let graph: Graph;
    try {
      graph = JSON.parse(runDeno(['info', '--json', '--frozen', '--node-modules-dir=manual', '--config', config.configPath, entry], repoRoot)) as Graph;
    } finally { fs.rmSync(entry, { force: true }); }
    if (!Array.isArray(graph.modules)) throw new Error('Unsupported deno info JSON: missing modules array');
    this.redirects = graph.redirects ?? {};
    for (const module of graph.modules) {
      this.modules.set(module.specifier, module);
      if (module.error) this.diagnostics.push(`${module.specifier}: ${module.error}`);
      if (!module.local) continue;
      let local = module.local;
      if (!module.specifier.startsWith('file:')) {
        const extensions: Record<string, string> = { TypeScript: '.ts', Tsx: '.tsx', JavaScript: '.js', Jsx: '.jsx', Dts: '.d.ts', Json: '.json', Mts: '.mts', Cts: '.cts' };
        const extension = extensions[module.mediaType ?? ''];
        if (!extension) { this.diagnostics.push(`${module.specifier}: unsupported media type ${module.mediaType}`); continue; }
        local = path.join(cache, 'remote', createHash('sha256').update(module.specifier).digest('hex') + extension);
        fs.mkdirSync(path.dirname(local), { recursive: true });
        fs.copyFileSync(module.local, local);
      }
      this.localPaths.set(module.specifier, path.resolve(local));
    }
    for (const module of graph.modules) {
      const local = this.localPaths.get(module.specifier);
      if (!local) continue;
      const edges = new Map<string, Resolution>();
      for (const dep of module.dependencies ?? []) {
        const target = dep.type ?? dep.code;
        if (target) edges.set(dep.specifier, target);
        if (target?.error) this.diagnostics.push(`${module.specifier}: ${dep.specifier}: ${target.error}`);
      }
      this.edges.set(local, edges);
    }
    // Deno's extra type roots must participate in both compiler programs.
    const entryModule = this.modules.get(pathToFileURL(entry).href);
    for (const dep of entryModule?.dependencies ?? []) {
      if (Array.isArray(extraTypes) && extraTypes.includes(dep.specifier)) {
        const local = this.targetPath(dep.type ?? dep.code);
        if (local) this.globals.push(local);
      }
    }
    fs.writeFileSync(path.join(cache, 'resolution-diagnostics.json'), JSON.stringify(this.diagnostics, null, 2));
    this.parsed.fileNames.push(...this.globals);
  }

  private targetPath(resolution?: Resolution): string | undefined {
    if (!resolution?.specifier || resolution.error) return undefined;
    let spec = resolution.specifier;
    const seen = new Set<string>();
    while (!seen.has(spec)) {
      seen.add(spec);
      const next = this.redirects[spec] ?? this.modules.get(spec)?.typesDependency?.dependency.specifier;
      if (!next) break;
      spec = next;
    }
    return this.localPaths.get(spec) ?? (spec.startsWith('file:') ? fileURLToPath(spec) : undefined);
  }

  resolve(spec: string, from: string, options: ts.CompilerOptions, host: ts.ModuleResolutionHost = ts.sys): ts.ResolvedModule | undefined {
    const edge = this.edges.get(path.resolve(from))?.get(spec);
    if (edge) {
      const target = this.targetPath(edge);
      if (!target || !fs.existsSync(target)) return undefined;
      if (!/\.(?:[cm]?tsx?|jsx?|json)$/i.test(target)) return undefined;
      return { resolvedFileName: target, isExternalLibraryImport: target.split(path.sep).includes('node_modules') };
    }
    // Generated capture imports and the internals of installed npm packages
    // use TypeScript resolution. Recorded failed Deno edges never fall back.
    return ts.resolveModuleName(spec, from, options, host).resolvedModule;
  }

  host(options: ts.CompilerOptions): ts.CompilerHost {
    const host = ts.createCompilerHost(options);
    host.resolveModuleNames = (names, from) => names.map(name => this.resolve(name, from, options, host));
    host.resolveTypeReferenceDirectives = (names, from) => names.map(name =>
      this.resolveTypeReference(typeof name === 'string' ? name : name.fileName, from, options, host));
    return host;
  }

  resolveTypeReference(spec: string, from: string, options: ts.CompilerOptions, host: ts.ModuleResolutionHost = ts.sys): ts.ResolvedTypeReferenceDirective | undefined {
    if (this.edges.get(path.resolve(from))?.has(spec)) {
      const resolved = this.resolve(spec, from, options, host);
      return resolved ? { resolvedFileName: resolved.resolvedFileName, primary: true } : undefined;
    }
    return ts.resolveTypeReferenceDirective(spec, from, options, host).resolvedTypeReferenceDirective;
  }

  /** Turn a Deno npm alias into its real package export, with the exact pin. */
  private externalName(spec: string, target: string): string | undefined {
    const key = `${spec}\0${target}`;
    if (this.externalNames.has(key)) return this.externalNames.get(key);
    let dir = path.dirname(target);
    while (dir.split(path.sep).includes('node_modules')) {
      const file = path.join(dir, 'package.json');
      if (fs.existsSync(file)) {
        const pkg = JSON.parse(fs.readFileSync(file, 'utf8')) as { name?: string; version?: string; exports?: unknown };
        if (pkg.name && pkg.version) {
          if (this.pinned[pkg.name] && this.pinned[pkg.name] !== pkg.version) {
            throw new Error(`Deno capture references multiple versions of ${pkg.name}; a single stub dependency cannot preserve both ${this.pinned[pkg.name]} and ${pkg.version}.`);
          }
          this.pinned[pkg.name] = pkg.version;
          const plain = spec.replace(/^npm:/, '').replace(/(@[^/]+\/[^/@]+|^[^/@]+)@[^/]+/, '$1');
          let result: string | undefined;
          if (plain === pkg.name || plain.startsWith(`${pkg.name}/`)) result = plain;
          else {
            const relative = './' + path.relative(dir, target).split(path.sep).join('/');
            const contains = (value: unknown): boolean => typeof value === 'string' ? value === relative : !!value && typeof value === 'object' && Object.values(value).some(contains);
            if (pkg.exports && typeof pkg.exports === 'object') {
              for (const [entry, value] of Object.entries(pkg.exports)) {
                if (entry.startsWith('.') && contains(value)) result = pkg.name + (entry === '.' ? '' : entry.slice(1));
              }
            }
            if (!result && contains(pkg.exports)) result = pkg.name;
          }
          if (result) this.externalNames.set(key, result);
          return result;
        }
      }
      dir = path.dirname(dir);
    }
    return undefined;
  }

  /** Relocate Deno's per-file resolutions into the portable declaration tree. */
  rewrite(typesDir: string, files: string[], sourceByEmitted: Map<string, string>): number {
    let count = 0;
    const emittedBySource = new Map([...sourceByEmitted].map(([rel, source]) => [path.resolve(source), rel]));
    for (const rel of files) {
      const from = sourceByEmitted.get(rel);
      if (!from) continue;
      const file = path.join(typesDir, rel);
      const result = rewriteSpecifiers(fs.readFileSync(file, 'utf8'), spec => {
        const target = this.resolve(spec, from, this.parsed.options)?.resolvedFileName;
        if (!target) return undefined;
        if (target.split(path.sep).includes('node_modules')) return this.externalName(spec, target);
        const dest = emittedBySource.get(path.resolve(target));
        if (!dest) return undefined;
        let relative = path.posix.relative(path.posix.dirname(rel), dest).replace(/\.d\.(ts|mts|cts)$/, '');
        if (!relative.startsWith('.')) relative = './' + relative;
        return relative;
      });
      if (result.rewrites) fs.writeFileSync(file, result.text);
      count += result.rewrites;
    }
    const runtime = this.globals.find(file => path.basename(file) === 'runtime.d.ts');
    const runtimeRel = runtime && emittedBySource.get(path.resolve(runtime));
    if (runtimeRel) isolateRuntime(typesDir, files, runtimeRel);
    const references = this.globals.filter(file => file !== runtime)
      .map(file => emittedBySource.get(path.resolve(file)))
      .filter((file): file is string => file !== undefined)
      .map(file => `/// <reference path=${JSON.stringify('./' + file)} />`);
    if (references.length) {
      const surface = path.join(typesDir, 'surface.d.ts');
      fs.writeFileSync(surface, references.join('\n') + '\n' + fs.readFileSync(surface, 'utf8'));
    }
    return count;
  }
}

/** Runtime declarations belong to their producer, not the checker's globals. */
function isolateRuntime(typesDir: string, files: string[], runtimeRel: string): void {
  const runtimePath = path.join(typesDir, runtimeRel);
  const program = ts.createProgram(files.map(file => path.join(typesDir, file)), {
    strict: true, skipLibCheck: true, target: ts.ScriptTarget.ESNext,
    module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler,
    types: [],
  });
  const checker = program.getTypeChecker();
  const runtime = program.getSourceFile(runtimePath)!;
  const retained = new Set<ts.Statement>();
  const pending: ts.Statement[] = [];
  const retainSymbol = (node: ts.Node): void => {
    if (ts.isIdentifier(node) && ts.isQualifiedName(node.parent) && node.parent.left === node) return;
    const symbol = checker.getSymbolAtLocation(node);
    // Runtime additions to standard library interfaces retain the compiler's
    // merged standard type; they cannot become a standalone shadow interface.
    if (symbol?.declarations?.some(d => program.isSourceFileDefaultLibrary(d.getSourceFile()))) return;
    for (const declaration of symbol?.declarations ?? []) {
      if (declaration.getSourceFile() !== runtime) continue;
      let statement: ts.Node = declaration;
      while (statement.parent && !ts.isSourceFile(statement.parent) && !ts.isModuleBlock(statement.parent)) statement = statement.parent;
      if (ts.isStatement(statement) && !retained.has(statement)) {
        retained.add(statement);
        pending.push(statement);
      }
    }
  };
  for (const rel of files) {
    if (rel === runtimeRel) continue;
    const file = path.join(typesDir, rel);
    const source = program.getSourceFile(file);
    if (!source) continue;
    let specifier = path.posix.relative(path.posix.dirname(rel), runtimeRel).replace(/\.d\.ts$/, '');
    if (!specifier.startsWith('.')) specifier = './' + specifier;
    const edits: { start: number; end: number; text: string }[] = [];
    const visit = (node: ts.Node): void => {
      if (ts.isIdentifier(node) || ts.isQualifiedName(node)) retainSymbol(node);
      if (ts.isIdentifier(node)) {
        const symbol = checker.getSymbolAtLocation(node);
        const declarations = symbol?.declarations;
        if (declarations?.some(d => d.getSourceFile() === runtime) &&
            // Only a type's root identifier, never a property or a declaration.
            ((ts.isTypeReferenceNode(node.parent) && node.parent.typeName === node) ||
             (ts.isQualifiedName(node.parent) && node.parent.left === node) ||
             (ts.isTypeQueryNode(node.parent) && node.parent.exprName === node))) {
          edits.push({ start: node.getStart(source), end: node.getEnd(), text: `import(${JSON.stringify(specifier)}).${node.text}` });
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
    let text = source.text;
    for (const edit of edits.sort((a, b) => b.start - a.start)) text = text.slice(0, edit.start) + edit.text + text.slice(edit.end);
    if (edits.length) fs.writeFileSync(file, text);
  }
  while (pending.length) {
    const visit = (node: ts.Node): void => {
      if (ts.isIdentifier(node) || ts.isQualifiedName(node)) retainSymbol(node);
      ts.forEachChild(node, visit);
    };
    visit(pending.pop()!);
  }
  const prune = (statement: ts.Statement): ts.Statement | undefined => {
    if (ts.isModuleDeclaration(statement) && statement.body && ts.isModuleBlock(statement.body)) {
      const children = statement.body.statements.map(prune).filter((s): s is ts.Statement => s !== undefined);
      if (!children.length && !retained.has(statement)) return undefined;
      return ts.factory.updateModuleDeclaration(statement, statement.modifiers, statement.name,
        ts.factory.updateModuleBlock(statement.body, children));
    }
    return retained.has(statement) ? statement : undefined;
  };
  const statements = runtime.statements.map(prune).filter((s): s is ts.Statement => s !== undefined);
  const names = new Set<string>();
  for (const statement of statements) {
    if (ts.isVariableStatement(statement)) {
      for (const declaration of statement.declarationList.declarations) {
        if (ts.isIdentifier(declaration.name)) names.add(declaration.name.text);
      }
    } else if ((ts.isInterfaceDeclaration(statement) || ts.isTypeAliasDeclaration(statement) ||
                ts.isModuleDeclaration(statement) || ts.isClassDeclaration(statement) ||
                ts.isFunctionDeclaration(statement) || ts.isEnumDeclaration(statement)) && statement.name && ts.isIdentifier(statement.name)) {
      names.add(statement.name.text);
    }
  }
  const text = ts.createPrinter().printFile(ts.factory.updateSourceFile(runtime, statements));
  fs.writeFileSync(runtimePath, '// Runtime declarations from deno types. Copyright the Deno authors. MIT license.\n/// <reference lib="esnext" />\n' + text + `\nexport { ${[...names].sort().join(', ')} };\n`);
}
