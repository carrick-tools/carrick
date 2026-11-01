/**
 * A tsconfig project reads each `package.json` once, not once per program
 * rebuild.
 *
 * Under `module` `node16`..`nodenext`, and for every file under
 * `node_modules`, the compiler reads the `package.json` scope above each
 * source file to decide the file's format. It memoises those reads in the
 * module resolution cache its host hands it. The tsconfig project resolves
 * imports through its own `resolveModuleNames` (carrick#1619), and ts-morph
 * forwarded no cache with it, so every program rebuild after an edit (two per
 * retyped call) read and parsed the whole `package.json` chain again: 37% of
 * a retyped call in an 863-file program.
 *
 * The project now hands the compiler one cache that lives as long as the
 * project. Only its `package.json` entries are ever filled: the compiler
 * resolves no module name itself while the host resolves them, so every
 * module answer still comes from the resolution host. The test builds the
 * same project twice, with and without the cache, rebuilds each after edits,
 * and requires the same files, formats, module answers and diagnostics.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project, ts, type FileSystemHost, type ResolutionHostFactory } from 'ts-morph';
import { moduleFormatResolutionHost } from '../src/module-format.js';
import { ExternalImports } from '../src/origin.js';

const roots: string[] = [];
after(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

/** A NodeNext service importing one installed ESM package and one CJS package. */
function fixture(): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-pkgjson-cache-')));
  roots.push(root);
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'svc', version: '1.0.0', type: 'module' }),
    'tsconfig.json': JSON.stringify({
      compilerOptions: {
        target: 'ES2022',
        module: 'NodeNext',
        moduleResolution: 'NodeNext',
        strict: true,
        skipLibCheck: true,
      },
      include: ['src'],
    }),
    'node_modules/esm-lib/package.json': JSON.stringify({
      name: 'esm-lib',
      version: '1.0.0',
      type: 'module',
      exports: { '.': { import: { types: './index.d.ts' } } },
    }),
    'node_modules/esm-lib/index.d.ts': 'export interface Order { id: string; total: number }\n',
    'node_modules/cjs-lib/package.json': JSON.stringify({ name: 'cjs-lib', version: '1.0.0', types: 'index.d.ts' }),
    'node_modules/cjs-lib/index.d.ts': 'export interface Customer { name: string }\n',
    'src/orders.ts':
      'import type { Order } from "esm-lib";\nimport type { Customer } from "cjs-lib";\n' +
      'import { label } from "./util/label.js";\n' +
      'export const order: Order = { id: "1", total: 2 };\nexport const who: Customer = { name: label(order.id) };\n',
    'src/util/label.ts': 'export function label(id: string): string { return `#${id}`; }\n',
    'src/legacy/package.json': JSON.stringify({ type: 'commonjs' }),
    'src/legacy/old.ts': 'import type { Customer } from "cjs-lib";\nexport const c: Customer = { name: "x" };\n',
  };
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return root;
}

/** The real file system, counting reads of `package.json` files. */
function countingFileSystem(): { fileSystem: FileSystemHost; reads: () => number } {
  const base = new Project({ skipAddingFilesFromTsConfig: true, skipFileDependencyResolution: true }).getFileSystem();
  let reads = 0;
  const fileSystem = new Proxy(base, {
    get(target, prop, receiver) {
      const value = Reflect.get(target, prop, receiver) as unknown;
      if (typeof value !== 'function') return value;
      const fn = value.bind(target) as (...args: any[]) => any;
      if (prop !== 'readFileSync') return fn;
      return (p: string, e?: string) => {
        if (path.basename(p) === 'package.json') reads += 1;
        return fn(p, e);
      };
    },
  });
  return { fileSystem, reads: () => reads };
}

/** The resolution host the tsconfig project uses, minus the cache it hands the compiler. */
const withoutCache: ResolutionHostFactory = (host, getOptions) => {
  const { getModuleResolutionCache: _dropped, ...rest } = moduleFormatResolutionHost()(host, getOptions) as ReturnType<
    ResolutionHostFactory
  > & { getModuleResolutionCache?: unknown };
  return rest;
};

/** What a program decided: each file's format, each import's answer, and the diagnostics. */
function decisions(program: ts.Program): string[] {
  const rows: string[] = [];
  for (const file of program.getSourceFiles()) {
    if (program.isSourceFileDefaultLibrary(file)) continue;
    rows.push(`file ${file.fileName} format=${String(file.impliedNodeFormat)}`);
    const imports = (file as ts.SourceFile & { imports?: readonly ts.StringLiteralLike[] }).imports ?? [];
    for (const literal of imports) {
      const answer = (
        program as ts.Program & {
          getResolvedModuleFromModuleSpecifier(
            literal: ts.StringLiteralLike,
            file: ts.SourceFile
          ): ts.ResolvedModuleWithFailedLookupLocations | undefined;
        }
      ).getResolvedModuleFromModuleSpecifier(literal, file)?.resolvedModule;
      rows.push(`  import ${literal.text} -> ${answer?.resolvedFileName ?? 'unresolved'}`);
    }
  }
  for (const d of ts.getPreEmitDiagnostics(program)) {
    rows.push(`diag ${d.file?.fileName ?? ''}:${d.start ?? ''} ${d.code} ${ts.flattenDiagnosticMessageText(d.messageText, '\n')}`);
  }
  return rows;
}

/** Build, then edit and rebuild `edits` times; package.json reads and decisions per build. */
function builds(root: string, factory: ResolutionHostFactory, edits: number) {
  const { fileSystem, reads } = countingFileSystem();
  const imports = new ExternalImports();
  // The constructor builds the first program while it resolves the files the
  // tsconfig lists, so the first build's reads are counted from here. Later
  // builds count only the rebuild: an edit itself asks the language service,
  // whose own host is not covered (its syntax tree cache reads the edited
  // file's scope once).
  let before = reads();
  const project = new Project({
    fileSystem,
    tsConfigFilePath: path.join(root, 'tsconfig.json'),
    resolutionHost: imports.recording(factory),
  });
  const file = project.getSourceFileOrThrow(path.join(root, 'src/orders.ts'));
  const original = file.getFullText();
  const out: { reads: number; decided: string[] }[] = [];
  const build = (): void => {
    const program = project.getProgram().compilerObject;
    out.push({ reads: reads() - before, decided: decisions(program) });
  };
  build();
  for (let i = 0; i < edits; i++) {
    file.replaceWithText(`${original}\n// edit ${i}\n`);
    before = reads();
    build();
    file.replaceWithText(original);
    before = reads();
    build();
  }
  return out;
}

describe('the tsconfig project memoises package.json reads across program rebuilds', () => {
  it('reads each package.json in the first build only, and decides exactly as without the cache', () => {
    const root = fixture();
    const cached = builds(root, moduleFormatResolutionHost(), 2);
    const uncached = builds(root, withoutCache, 2);

    // The control shows the cost the cache removes: every rebuild reads again.
    assert.ok(uncached[0].reads > 0, 'the first build reads package.json files');
    for (const [i, b] of uncached.entries()) assert.ok(b.reads > 0, `uncached build ${i} reads package.json again`);

    assert.ok(cached[0].reads > 0, 'the first build reads package.json files');
    assert.deepStrictEqual(
      cached.slice(1).map((b) => b.reads),
      cached.slice(1).map(() => 0),
      'no rebuild reads a package.json the project already read'
    );

    // Same files, formats, module answers and diagnostics, build by build.
    assert.strictEqual(cached.length, uncached.length);
    for (const [i, b] of cached.entries()) assert.deepStrictEqual(b.decided, uncached[i].decided, `build ${i}`);
    // The fixture reaches both formats and both packages, so the comparison is not vacuous.
    const first = cached[0].decided.join('\n');
    assert.match(first, /src\/orders\.ts format=99/);
    assert.match(first, /src\/legacy\/old\.ts format=1/);
    assert.match(first, /import esm-lib -> .*node_modules\/esm-lib\/index\.d\.ts/);
    assert.match(first, /import cjs-lib -> .*node_modules\/cjs-lib\/index\.d\.ts/);
  });
});
