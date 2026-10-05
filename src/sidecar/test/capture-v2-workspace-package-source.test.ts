/**
 * A package of the scanned checkout is read from its source, and its
 * declarations travel in the stub (carrick#1910, carrick#1620, carrick#1175).
 *
 * In a monorepo a package's manifest points its entry at build output
 * (`dist/index.d.ts`). On a checkout that was installed and not built that
 * file is missing, so a service that imports a type from the package by name
 * got the compiler's unresolved placeholder: it prints the type's name and
 * every member reads `any`. An editor reads the package's project config for
 * where each source file's output goes and opens the source instead. The
 * capture does the same, and says so as the reason where no project of the
 * package writes the entry.
 *
 * Resolving is half of it. The stub pins nothing for a package nobody
 * published, so a surface that names `@depot/contracts` cannot typecheck in
 * the check's workspace. The package's declarations are emitted into the
 * stub, and every specifier that names them is rewritten to where they sit.
 *
 * And an anchor whose source arrives as the specifier its type was imported
 * by (a package name, a `paths` alias) is resolved as that module, where it
 * used to be joined onto the service root as if it were a file.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub, runCheck } from '../src/capture/index.js';
import type { CaptureAliasRecord, CaptureAnchorRequest, CaptureStubResult } from '../src/capture/api.js';
import { SidecarClient } from './helpers.js';

const BASE_OPTIONS = { target: 'ES2022', strict: true, skipLibCheck: true, declaration: true };

const CONTRACTS_SOURCES: Record<string, string> = {
  'packages/contracts/lib/index.ts': [
    "export type { Label } from './labels';",
    'export interface Crate { id: string; slots: number }',
    'export interface CrateStored { crate: Crate; at: string }',
    'export type CrateSummary = { id: string; full: boolean };',
    '',
  ].join('\n'),
  'packages/contracts/lib/labels.ts': 'export interface Label { crateId: string; code: string }\n',
};

const API_SOURCE = [
  "import type { Crate, CrateStored, CrateSummary } from '@depot/contracts';",
  '',
  'declare function publish(topic: string, payload: unknown): void;',
  '',
  'export interface Shelf { crates: Crate[]; aisle: number }',
  '',
  'export function stored(crate: Crate): CrateStored {',
  "  return { crate, at: 'now' };",
  '}',
  '',
  'export function announce(crate: Crate): void {',
  '  const event = { crate, count: 1 };',
  "  publish('crate.stored', event);",
  '}',
  '',
  'export async function summarise(crate: Crate): Promise<CrateSummary> {',
  '  return { id: crate.id, full: crate.slots === 0 };',
  '}',
  '',
].join('\n');

/** The 1-based line of `API_SOURCE` that holds `text`. */
const lineOf = (text: string): number => API_SOURCE.split('\n').findIndex((line) => line.includes(text)) + 1;
const EVENT_LINE = lineOf('const event');
const SUMMARISE_LINE = lineOf('export async function summarise');

interface Layout {
  /** The package's manifest, beside its name and version. */
  manifest: Record<string, unknown>;
  /** Files of the package beside its sources: its project configs. */
  files: Record<string, string>;
  /** `compilerOptions` of the service, beside the base. */
  service?: Record<string, unknown>;
  /** The service's `references`. */
  references?: { path: string }[];
  /** The specifier the service imports the package by. */
  specifier?: string;
  /** Both packages are ES modules (`"type": "module"`). */
  esm?: true;
}

const tsconfig = (compilerOptions: Record<string, unknown>, rest: Record<string, unknown> = {}): string =>
  JSON.stringify({ compilerOptions: { ...BASE_OPTIONS, ...compilerOptions }, ...rest });

/** `rootDir` and `outDir` in the package's own tsconfig, referenced by the service. */
const REFERENCED: Layout = {
  manifest: { types: 'dist/index.d.ts', main: 'dist/index.js' },
  files: {
    'packages/contracts/tsconfig.json': tsconfig({ composite: true, rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
  },
  references: [{ path: '../contracts' }],
};

const bases: string[] = [];
after(() => {
  for (const base of bases) fs.rmSync(base, { recursive: true, force: true });
});

function writeRepo(layout: Layout, extra: Record<string, string> = {}): { base: string; repo: string; service: string } {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1910-')));
  bases.push(base);
  const repo = path.join(base, 'repo');
  const type = layout.esm ? { type: 'module' } : {};
  const sources = Object.fromEntries(
    // An ES module names the file its import compiles to.
    Object.entries(CONTRACTS_SOURCES).map(([rel, text]) => [rel, layout.esm ? text.replace("'./labels'", "'./labels.js'") : text])
  );
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'depot', private: true, workspaces: ['packages/*'] }),
    'packages/contracts/package.json': JSON.stringify({ name: '@depot/contracts', version: '1.0.0', ...type, ...layout.manifest }),
    ...sources,
    ...layout.files,
    'packages/api/package.json': JSON.stringify({
      name: '@depot/api',
      version: '1.0.0',
      ...type,
      dependencies: { '@depot/contracts': '1.0.0' },
    }),
    'packages/api/tsconfig.json': tsconfig(
      { module: 'CommonJS', rootDir: 'lib', outDir: 'dist', ...layout.service },
      { include: ['lib'], ...(layout.references ? { references: layout.references } : {}) }
    ),
    'packages/api/lib/crates.ts': API_SOURCE.replace("'@depot/contracts'", `'${layout.specifier ?? '@depot/contracts'}'`),
    ...extra,
  };
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true });
    fs.writeFileSync(path.join(repo, rel), text);
  }
  // What an install leaves: the workspace's packages linked by name.
  fs.mkdirSync(path.join(repo, 'node_modules/@depot'), { recursive: true });
  fs.symlinkSync('../../packages/contracts', path.join(repo, 'node_modules/@depot/contracts'), 'dir');
  return { base, repo, service: path.join(repo, 'packages/api') };
}

const ANCHORS: CaptureAnchorRequest[] = [
  // The payload a call site hands on, located by its expression.
  {
    kind: 'infer',
    alias: 'Endpoint_event_Payload',
    source_file: 'lib/crates.ts',
    anchor_origin: 'deterministic-infer',
    line_number: EVENT_LINE,
    expression_text: 'event',
  },
  // The same payload as text the inference printed.
  {
    kind: 'literal',
    alias: 'Endpoint_event_Text',
    type_text: '{ crate: Crate; count: number; }',
    source_file: 'lib/crates.ts',
    anchor_origin: 'deterministic-infer',
  },
  // A type the service declares over the package's.
  { kind: 'symbol', alias: 'Endpoint_shelf_Response', symbol_name: 'Shelf', source_file: 'lib/crates.ts', anchor_origin: 'llm-symbol' },
  { kind: 'handler_return', alias: 'Endpoint_stored_Response', symbol_name: 'stored', source_file: 'lib/crates.ts', anchor_origin: 'llm-symbol' },
];

/** A type of the package, by the name the service imports it under. */
const byPackageName = (specifier: string): CaptureAnchorRequest => ({
  kind: 'symbol',
  alias: 'Endpoint_crate_Response',
  symbol_name: 'Crate',
  source_file: specifier,
  anchor_origin: 'llm-symbol',
});

function capture(repo: string, service: string, base: string, anchors: CaptureAnchorRequest[]): CaptureStubResult {
  const result = captureStub({
    repoRoot: service,
    scanRoot: repo,
    serviceName: 'api',
    outDir: path.join(base, 'stub'),
    anchors,
  });
  assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
  return result;
}

const summary = (aliases: CaptureAliasRecord[]) =>
  aliases.map((a) => [a.alias, a.self_check, a.self_check_detail]);

/**
 * A copy of the stub's declarations with nothing else beside it, typechecked
 * as the check's workspace does: what the stub cannot say for itself, it does
 * not say.
 */
function standalone(stubDir: string, base: string) {
  const copy = path.join(base, 'copy');
  fs.cpSync(path.join(stubDir, 'types'), path.join(copy, 'types'), { recursive: true });
  const surface = path.join(copy, 'types', 'surface.d.ts');
  const program = ts.createProgram([surface], {
    strict: true,
    noEmit: true,
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    types: [],
  });
  const checker = program.getTypeChecker();
  const source = program.getSourceFile(surface)!;
  const typeOf = (alias: string): ts.Type =>
    checker.getTypeAtLocation(
      source.statements.find(
        (s): s is ts.TypeAliasDeclaration => ts.isTypeAliasDeclaration(s) && s.name.text === alias
      )!
    );
  const names = (type: ts.Type): string[] => type.getProperties().map((p) => p.name).sort();
  return {
    diagnostics: ts
      .getPreEmitDiagnostics(program)
      .map((d) => `${d.file ? path.relative(copy, d.file.fileName) : ''}: ${ts.flattenDiagnosticMessageText(d.messageText, ' ')}`),
    outside: program
      .getSourceFiles()
      .filter((f) => !program.isSourceFileDefaultLibrary(f) && !f.fileName.startsWith(copy))
      .map((f) => f.fileName),
    surface: fs.readFileSync(surface, 'utf8'),
    members: (alias: string) => names(typeOf(alias)),
    /** Member names of the type at `alias.member`. */
    memberOf: (alias: string, member: string) => {
      const property = checker.getPropertyOfType(typeOf(alias), member)!;
      return names(checker.getTypeOfSymbol(property));
    },
  };
}

/** Every alias reads the package's types, and the stub carries them. */
function assertCarried(result: CaptureStubResult, base: string, anchors: CaptureAnchorRequest[]): void {
  assert.deepStrictEqual(
    summary(result.aliases),
    anchors.map((a) => [a.alias, 'ok', undefined]),
    JSON.stringify(result.aliases, null, 1)
  );
  // Nothing published the package, so the stub cannot name it.
  assert.deepStrictEqual(result.unpinned_externals, []);
  assert.deepStrictEqual(result.pinned_dependencies, {});
  const stub = standalone(result.stub_dir, base);
  assert.ok(!stub.surface.includes('@depot/contracts'), stub.surface);
  assert.deepStrictEqual(stub.diagnostics, []);
  assert.deepStrictEqual(stub.outside, []);
  assert.deepStrictEqual(stub.memberOf('Endpoint_event_Payload', 'crate'), ['id', 'slots']);
  assert.deepStrictEqual(stub.memberOf('Endpoint_event_Text', 'crate'), ['id', 'slots']);
  assert.deepStrictEqual(stub.members('Endpoint_shelf_Response'), ['aisle', 'crates']);
  assert.deepStrictEqual(stub.memberOf('Endpoint_stored_Response', 'crate'), ['id', 'slots']);
}

describe('carrick#1910: an unbuilt package of the checkout is read from its source', () => {
  it('the ticket: the entry is output of the project the service references', () => {
    const { base, repo, service } = writeRepo(REFERENCED);
    const anchors = [...ANCHORS, byPackageName('@depot/contracts')];
    const result = capture(repo, service, base, anchors);
    assertCarried(result, base, anchors);
    assert.deepStrictEqual(standalone(result.stub_dir, base).members('Endpoint_crate_Response'), ['id', 'slots']);
    // The package's declarations are in the stub, under the tree.
    assert.ok(
      result.emitted_files.some((file) => /^types\/__outside__\/.*contracts\/lib\/index\.d\.ts$/.test(file)),
      result.emitted_files.join(', ')
    );
    assert.ok(result.emitted_files.every((file) => file.startsWith('types/') && !file.includes('..')));
  });

  it('ES-module packages under nodenext, the shape the ticket was found on', () => {
    // The service's own modules are named by their output file here
    // (carrick#1911), and the package's by where its declarations sit.
    const { base, repo, service } = writeRepo({
      manifest: { types: 'dist/index.d.ts' },
      files: {
        'packages/contracts/tsconfig.json': tsconfig(
          { module: 'NodeNext', composite: true, rootDir: 'lib', outDir: 'dist' },
          { include: ['lib'] }
        ),
      },
      service: { module: 'NodeNext' },
      references: [{ path: '../contracts' }],
      esm: true,
    });
    const anchors = [
      ...ANCHORS,
      byPackageName('@depot/contracts'),
      { kind: 'symbol' as const, alias: 'Endpoint_label_Response', symbol_name: 'Label', source_file: '@depot/contracts', anchor_origin: 'llm-symbol' as const },
    ];
    const result = capture(repo, service, base, anchors);
    assertCarried(result, base, anchors);
    assert.deepStrictEqual(standalone(result.stub_dir, base).members('Endpoint_label_Response'), ['code', 'crateId']);
  });

  it('a project config the service does not reference, named for a build', () => {
    // `tsc -p tsconfig.build.json` writes `dist/lib/index.d.ts`: no `rootDir`,
    // and a composite project's root is its config's directory.
    const { base, repo, service } = writeRepo({
      manifest: { types: './dist/lib/index.d.ts' },
      files: {
        'packages/contracts/tsconfig.json': tsconfig({ noEmit: true }, { include: ['lib'] }),
        'packages/contracts/tsconfig.build.json': tsconfig({ composite: true, outDir: 'dist' }, { include: ['lib'] }),
      },
    });
    assertCarried(capture(repo, service, base, ANCHORS), base, ANCHORS);
  });

  it('a project config reached through a solution config in the package', () => {
    const { base, repo, service } = writeRepo({
      manifest: { types: './dist/index.d.ts' },
      files: {
        'packages/contracts/tsconfig.json': JSON.stringify({ files: [], references: [{ path: './config/tsconfig.lib.json' }] }),
        'packages/contracts/config/tsconfig.lib.json': tsconfig(
          { composite: true, rootDir: '../lib', outDir: '../dist' },
          { include: ['../lib'] }
        ),
      },
    });
    assertCarried(capture(repo, service, base, ANCHORS), base, ANCHORS);
  });

  it('an exports map, read in the mode of the import', () => {
    const { base, repo, service } = writeRepo({
      manifest: {
        exports: {
          '.': { types: './dist/index.d.ts', default: './dist/index.js' },
          './labels': { types: './dist/labels.d.ts', default: './dist/labels.js' },
        },
      },
      files: {
        'packages/contracts/tsconfig.json': tsconfig({ rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
      },
      service: { module: 'NodeNext' },
    });
    const anchors: CaptureAnchorRequest[] = [
      ...ANCHORS,
      { kind: 'symbol', alias: 'Endpoint_label_Response', symbol_name: 'Label', source_file: '@depot/contracts/labels', anchor_origin: 'llm-symbol' },
    ];
    const result = capture(repo, service, base, anchors);
    assertCarried(result, base, anchors);
    assert.deepStrictEqual(standalone(result.stub_dir, base).members('Endpoint_label_Response'), ['code', 'crateId']);
  });

  it('a manifest that names only the script: its declaration sits beside it', () => {
    const { base, repo, service } = writeRepo({
      manifest: { main: './dist/index.js' },
      files: {
        'packages/contracts/tsconfig.json': tsconfig({ declaration: false, rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
      },
    });
    assertCarried(capture(repo, service, base, ANCHORS), base, ANCHORS);
  });

  it('a package whose entry is already source travels in the stub too (carrick#1620)', () => {
    const { base, repo, service } = writeRepo({ manifest: { types: 'lib/index.ts' }, files: {} });
    const anchors = [...ANCHORS, byPackageName('@depot/contracts')];
    assertCarried(capture(repo, service, base, anchors), base, anchors);
  });

  it('travels when only a file of the service names it', () => {
    // No alias names the package itself: the surface reaches it through the
    // service's own module, which imports it by name. The compiler marks what
    // it reaches through `node_modules` as a library and emits nothing for it.
    const { base, repo, service } = writeRepo({ manifest: { types: 'lib/index.ts' }, files: {} });
    const anchors = [ANCHORS[2], ANCHORS[3]];
    const result = capture(repo, service, base, anchors);
    assert.deepStrictEqual(summary(result.aliases), anchors.map((a) => [a.alias, 'ok', undefined]));
    assert.deepStrictEqual(result.unpinned_externals, []);
    const stub = standalone(result.stub_dir, base);
    assert.deepStrictEqual(stub.diagnostics, []);
    assert.deepStrictEqual(stub.outside, []);
    assert.deepStrictEqual(stub.memberOf('Endpoint_stored_Response', 'crate'), ['id', 'slots']);
  });

  it('a package that was built is read as built', () => {
    const { base, repo, service } = writeRepo(REFERENCED, {
      // A build that differs from the source, so the two cannot be confused.
      'packages/contracts/dist/index.d.ts': 'export interface Crate { built: true }\nexport interface CrateStored { crate: Crate }\n',
    });
    const result = capture(repo, service, base, [ANCHORS[0]]);
    assert.match(standalone(result.stub_dir, base).surface, /crate: import\("@depot\/contracts"\)\.Crate/);
    assert.ok(!result.emitted_files.some((file) => file.includes('__outside__')), result.emitted_files.join(', '));
  });
});

describe('carrick#1910: where no source can be found, the reason says so', () => {
  const NOTE =
    "'@depot/contracts' is a package of this checkout whose entry is not on disk, " +
    'and no tsconfig in the package writes that entry from a source file';

  it('no project of the package writes the entry', () => {
    // A bundler writes `dist/index.d.ts`; the tsconfig only typechecks.
    const { base, repo, service } = writeRepo({
      manifest: { types: 'dist/index.d.ts' },
      files: { 'packages/contracts/tsconfig.json': tsconfig({ noEmit: true }, { include: ['lib'] }) },
    });
    const anchors = [ANCHORS[0], ANCHORS[2], byPackageName('@depot/contracts')];
    const result = capture(repo, service, base, anchors);
    const byAlias = new Map(result.aliases.map((a) => [a.alias, a]));

    for (const alias of ['Endpoint_event_Payload', 'Endpoint_shelf_Response']) {
      const record = byAlias.get(alias)!;
      assert.strictEqual(record.self_check, 'decayed_internal', JSON.stringify(record));
      const finding = record.any_provenance?.[0];
      assert.strictEqual(finding?.reason, 'unresolved_import', JSON.stringify(record));
      assert.ok(finding?.detail?.endsWith(`; ${NOTE}`), finding?.detail);
    }
    const named = byAlias.get('Endpoint_crate_Response')!;
    assert.strictEqual(named.self_check, 'decayed_internal');
    assert.strictEqual(named.capture_failure_reason, `source '@depot/contracts' did not resolve: ${NOTE}`);
    assert.strictEqual(standalone(result.stub_dir, base).surface.includes('any'), false);
  });

  it('two projects of the package write the entry from different sources', () => {
    const { base, repo, service } = writeRepo(
      {
        manifest: { types: 'dist/index.d.ts' },
        files: {
          'packages/contracts/tsconfig.a.json': tsconfig({ rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
          'packages/contracts/tsconfig.b.json': tsconfig({ rootDir: 'alt', outDir: 'dist' }, { include: ['alt'] }),
        },
      },
      { 'packages/contracts/alt/index.ts': 'export interface Crate { other: boolean }\n' }
    );
    const result = capture(repo, service, base, [ANCHORS[0]]);
    const record = result.aliases[0];
    assert.strictEqual(record.self_check, 'decayed_internal', JSON.stringify(record));
    assert.match(
      record.any_provenance?.[0]?.detail ?? '',
      /two tsconfigs in the package write that entry from different source files$/
    );
  });

  it('an installed package that ships its source and no build is not read from it', () => {
    // Only the checkout's own packages are followed to source: a dependency
    // somebody published is read as it was installed.
    const { base, repo, service } = writeRepo(REFERENCED, {
      'node_modules/cratekit/package.json': JSON.stringify({ name: 'cratekit', version: '2.3.4', types: 'dist/index.d.ts' }),
      'node_modules/cratekit/tsconfig.json': tsconfig({ rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
      'node_modules/cratekit/lib/index.ts': 'export interface Crate { id: string; slots: number }\n',
      'packages/api/lib/crates.ts': API_SOURCE.replace("'@depot/contracts'", "'cratekit'"),
    });
    const result = capture(repo, service, base, [ANCHORS[0]]);
    const record = result.aliases[0];
    assert.strictEqual(record.self_check, 'decayed_internal', JSON.stringify(record));
    assert.match(record.any_provenance?.[0]?.detail ?? '', /unresolved imports reachable from the anchor: 'cratekit'$/);
  });

  it('a dependency that is not installed keeps the sentence it had', () => {
    const { base, repo, service } = writeRepo(REFERENCED, {
      'packages/api/lib/crates.ts': API_SOURCE.replace("'@depot/contracts'", "'cratekit'"),
    });
    const result = capture(repo, service, base, [ANCHORS[0], byPackageName('cratekit')]);
    const [inferred, named] = result.aliases;
    assert.match(inferred.any_provenance?.[0]?.detail ?? '', /unresolved imports reachable from the anchor: 'cratekit'$/);
    assert.strictEqual(named.capture_failure_reason, 'source file not in program: cratekit');
  });
});

describe('carrick#1175: an anchor source that is a module specifier is resolved as one', () => {
  it('a `paths` alias names the file it maps to', () => {
    const { base, repo, service } = writeRepo(
      { ...REFERENCED, service: { paths: { '@/*': ['./lib/*'] } } },
      { 'packages/api/lib/shared/types.ts': 'export interface Aisle { number: number; cold: boolean }\n' }
    );
    const result = capture(repo, service, base, [
      { kind: 'symbol', alias: 'Endpoint_aisle_Response', symbol_name: 'Aisle', source_file: '@/shared/types', anchor_origin: 'llm-symbol' },
    ]);
    assert.deepStrictEqual(summary(result.aliases), [['Endpoint_aisle_Response', 'ok', undefined]]);
    const stub = standalone(result.stub_dir, base);
    assert.deepStrictEqual(stub.diagnostics, []);
    assert.deepStrictEqual(stub.members('Endpoint_aisle_Response'), ['cold', 'number']);
  });

  it('an installed package is named by its bare specifier and pinned', () => {
    const { base, repo, service } = writeRepo(REFERENCED, {
      'node_modules/cratekit/package.json': JSON.stringify({ name: 'cratekit', version: '2.3.4', types: 'index.d.ts' }),
      'node_modules/cratekit/index.d.ts': 'export interface Reply { status: number; body: string }\n',
    });
    const result = capture(repo, service, base, [
      { kind: 'symbol', alias: 'Endpoint_reply_Response', symbol_name: 'Reply', source_file: 'cratekit', anchor_origin: 'llm-symbol' },
    ]);
    assert.deepStrictEqual(summary(result.aliases), [['Endpoint_reply_Response', 'ok', undefined]]);
    assert.deepStrictEqual(result.pinned_dependencies, { cratekit: '2.3.4' });
    assert.match(fs.readFileSync(path.join(result.stub_dir, 'types/surface.d.ts'), 'utf8'), /import\(['"]cratekit['"]\)\.Reply/);
  });
});

describe('carrick#1175: a service below the scanned root gets the specifier joined onto that root', () => {
  // The scanner hands on a specifier it found no file for joined onto the
  // root it scans. The service here sits two directories below that root, so
  // the path arrives whole, and names nothing on disk.
  const joined = (repo: string, specifier: string): CaptureAnchorRequest => ({
    kind: 'symbol',
    alias: 'Endpoint_joined_Response',
    symbol_name: specifier === 'cratekit' ? 'Reply' : 'Crate',
    source_file: path.join(repo, specifier),
    anchor_origin: 'llm-symbol',
  });

  it('a package of the checkout is read as the package the specifier names', () => {
    const { base, repo, service } = writeRepo(REFERENCED);
    assert.ok(!fs.existsSync(path.join(repo, '@depot/contracts')));
    const result = capture(repo, service, base, [joined(repo, '@depot/contracts')]);
    assert.deepStrictEqual(summary(result.aliases), [['Endpoint_joined_Response', 'ok', undefined]], JSON.stringify(result.aliases, null, 1));
    const stub = standalone(result.stub_dir, base);
    assert.deepStrictEqual(stub.diagnostics, []);
    assert.deepStrictEqual(stub.members('Endpoint_joined_Response'), ['id', 'slots']);
  });

  it('an installed package is named by its bare specifier and pinned', () => {
    const { base, repo, service } = writeRepo(REFERENCED, {
      'node_modules/cratekit/package.json': JSON.stringify({ name: 'cratekit', version: '2.3.4', types: 'index.d.ts' }),
      'node_modules/cratekit/index.d.ts': 'export interface Reply { status: number; body: string }\n',
    });
    const result = capture(repo, service, base, [joined(repo, 'cratekit')]);
    assert.deepStrictEqual(summary(result.aliases), [['Endpoint_joined_Response', 'ok', undefined]], JSON.stringify(result.aliases, null, 1));
    assert.deepStrictEqual(result.pinned_dependencies, { cratekit: '2.3.4' });
  });

  it('a package with no source to read says so by its name', () => {
    const { base, repo, service } = writeRepo({ manifest: { types: 'dist/index.d.ts' }, files: {} });
    const [record] = capture(repo, service, base, [joined(repo, '@depot/contracts')]).aliases;
    assert.match(record.capture_failure_reason ?? '', /^source '@depot\/contracts' did not resolve: '@depot\/contracts' is a package of this checkout whose entry is not on disk/);
  });

  it('a path that names a file is still read as that file', () => {
    const { base, repo, service } = writeRepo(REFERENCED);
    const result = capture(repo, service, base, [
      { ...joined(repo, '@depot/contracts'), source_file: path.join(repo, 'packages/contracts/lib/index.ts') },
    ]);
    assert.deepStrictEqual(summary(result.aliases), [['Endpoint_joined_Response', 'ok', undefined]], JSON.stringify(result.aliases, null, 1));
  });

  it('a path outside the scanned root is not read as a specifier', () => {
    const { base, repo, service } = writeRepo(REFERENCED);
    const [record] = capture(repo, service, base, [
      { ...joined(repo, '@depot/contracts'), source_file: path.join(base, '@depot/contracts') },
    ]).aliases;
    assert.match(record.capture_failure_reason ?? '', /^source file not in program: /);
  });
});

describe('carrick#1910: the init\'d project reads the package from source too', () => {
  /** What `infer` answers for the return of `summarise`, a type alias of the package. */
  async function inferSummary(layout: Layout): Promise<{ type_string: string; any_provenance?: unknown[] }> {
    const { repo, service } = writeRepo(layout);
    const client = new SidecarClient();
    await client.start();
    try {
      await client.send({ action: 'init', request_id: 'init', repo_root: service, scan_root: repo, tsconfig_path: 'tsconfig.json' });
      const answer = await client.send<{ inferred_types?: Array<{ type_string: string; any_provenance?: unknown[] }> }>(
        {
          action: 'infer',
          request_id: 'infer',
          requests: [
            {
              file_path: path.join(service, 'lib/crates.ts'),
              line_number: SUMMARISE_LINE,
              infer_kind: 'function_return',
              alias: 'Summary',
            },
          ],
        },
        60000
      );
      return answer.inferred_types![0];
    } finally {
      await client.stop();
    }
  }

  it('prints the members of a type the unbuilt package declares', async () => {
    const answer = await inferSummary(REFERENCED);
    assert.strictEqual(answer.type_string.replace(/\s+/g, ' '), '{ id: string; full: boolean; }');
    assert.strictEqual(answer.any_provenance, undefined);
  });

  it('leaves the name, with its provenance, where the package has no source to read', async () => {
    const answer = await inferSummary({
      manifest: { types: 'dist/index.d.ts' },
      files: { 'packages/contracts/tsconfig.json': tsconfig({ noEmit: true }, { include: ['lib'] }) },
    });
    assert.strictEqual(answer.type_string, 'CrateSummary');
    assert.ok(answer.any_provenance, 'an unresolved return must carry its provenance');
  });
});

describe('carrick#1620: a stub that carries the package is judged on its types', () => {
  it('reads compatible against a matching consumer and incompatible against a differing one', async () => {
    const { base, repo, service } = writeRepo(REFERENCED);
    const producer = capture(repo, service, base, ANCHORS);
    const consumer = path.join(base, 'consumer');
    fs.mkdirSync(path.join(consumer, 'types'), { recursive: true });
    fs.writeFileSync(
      path.join(consumer, 'package.json'),
      JSON.stringify({ name: '@carrick/web', version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' })
    );
    fs.writeFileSync(
      path.join(consumer, 'types/surface.d.ts'),
      [
        'export type Same = { crate: { id: string; slots: number }; at: string };',
        'export type Differs = { crate: { id: number; slots: number }; at: string };',
        '',
      ].join('\n')
    );
    const result = await runCheck({
      stubs: [
        { service_name: 'api', stub_dir: producer.stub_dir },
        { service_name: 'web', stub_dir: consumer },
      ],
      pairs: ['Same', 'Differs'].map((alias) => ({
        pair_key: alias,
        protocol: 'http' as const,
        type_kind: 'response' as const,
        producer: { service_name: 'api', alias: 'Endpoint_stored_Response' },
        consumer: { service_name: 'web', alias },
      })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const byKey = new Map(result.verdicts.map((v) => [v.pair_key, v]));
    assert.strictEqual(byKey.get('Same')!.bucket, 'compatible', JSON.stringify(byKey.get('Same')));
    assert.strictEqual(byKey.get('Differs')!.bucket, 'incompatible', JSON.stringify(byKey.get('Differs')));
  });
});

/**
 * A workspace with an isolated install, as pnpm lays one out: the service
 * links `@depot/records` and nothing else, and what the package depends on is
 * linked into the package's own `node_modules`. The package is not built.
 */
function isolatedWorkspace(
  records: Record<string, string>,
  service: Record<string, string>,
  extra: { files?: Record<string, string>; links?: Record<string, string>; manifest?: Record<string, unknown> } = {}
): { base: string; repo: string; service: string } {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1910-')));
  bases.push(base);
  const repo = path.join(base, 'repo');
  const units = 'node_modules/.pnpm/units@3.1.0/node_modules/units';
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'depot', private: true }),
    [`${units}/package.json`]: JSON.stringify({ name: 'units', version: '3.1.0', types: 'index.d.ts' }),
    [`${units}/index.d.ts`]: 'export interface Unit { symbol: string; factor: number }\n',
    'packages/records/package.json': JSON.stringify({
      name: '@depot/records',
      version: '1.0.0',
      types: 'dist/index.d.ts',
      main: 'dist/index.js',
      dependencies: { units: '3.1.0' },
      ...extra.manifest,
    }),
    'packages/records/tsconfig.json': tsconfig({ module: 'CommonJS', rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
    ...Object.fromEntries(Object.entries(records).map(([rel, text]) => [`packages/records/${rel}`, text])),
    'packages/api/package.json': JSON.stringify({ name: '@depot/api', version: '1.0.0', dependencies: { '@depot/records': 'workspace:*' } }),
    'packages/api/tsconfig.json': tsconfig({ module: 'CommonJS', rootDir: 'lib' }, { include: ['lib'] }),
    ...Object.fromEntries(Object.entries(service).map(([rel, text]) => [`packages/api/${rel}`, text])),
    ...extra.files,
  };
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true });
    fs.writeFileSync(path.join(repo, rel), text);
  }
  const link = (at: string, to: string) => {
    fs.mkdirSync(path.dirname(path.join(repo, at)), { recursive: true });
    fs.symlinkSync(path.relative(path.dirname(path.join(repo, at)), path.join(repo, to)), path.join(repo, at), 'dir');
  };
  link('packages/records/node_modules/units', units);
  link('packages/api/node_modules/@depot/records', 'packages/records');
  for (const [at, to] of Object.entries(extra.links ?? {})) link(at, to);
  return { base, repo, service: path.join(repo, 'packages/api') };
}

/** A capture that reads no package of the checkout from source: the service is its whole scope. */
function captureAsBefore(service: string, base: string, anchors: CaptureAnchorRequest[]): CaptureStubResult {
  const result = captureStub({ repoRoot: service, scanRoot: service, serviceName: 'api', outDir: path.join(base, 'before'), anchors });
  assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
  return result;
}

/** Every file of a stub's declarations, with its text. */
function declarations(stubDir: string): Record<string, string> {
  const types = path.join(stubDir, 'types');
  const out: Record<string, string> = {};
  const walk = (dir: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(file);
      else out[path.relative(types, file)] = fs.readFileSync(file, 'utf8');
    }
  };
  walk(types);
  return out;
}

describe('carrick#1910: a package whose source is not all on the checkout is read as it was', () => {
  // The service's result type names nothing of the package; the class beside
  // it takes two of the package's types, one of them from the missing module.
  const TALLY = {
    'lib/tally.ts': [
      "import type { Row, Store } from '@depot/records';",
      'export type Tally = { count: number };',
      'export class Counter {',
      '  constructor(private readonly store: Store) {}',
      '  count(row: Row): Tally {',
      '    return { count: row ? 1 : 0 };',
      '  }',
      '}',
      '',
    ].join('\n'),
  };
  const TALLY_ANCHORS: CaptureAnchorRequest[] = [
    { kind: 'symbol', alias: 'Endpoint_tally_Response', symbol_name: 'Tally', source_file: 'lib/tally.ts', anchor_origin: 'llm-symbol' },
    { kind: 'symbol', alias: 'Endpoint_counter_Response', symbol_name: 'Counter', source_file: 'lib/tally.ts', anchor_origin: 'llm-symbol' },
  ];
  const STORE = 'export interface Store { name: string }\n';

  /** The capture says of the package exactly what one that never read it from source says. */
  function assertAsBefore(
    records: Record<string, string>,
    because: string,
    extra: Parameters<typeof isolatedWorkspace>[2] = {}
  ): void {
    const { base, repo, service } = isolatedWorkspace(records, TALLY, extra);
    const before = captureAsBefore(service, base, TALLY_ANCHORS);
    const result = capture(repo, service, base, TALLY_ANCHORS);
    // The result type read `ok` before, and a package that cannot be read
    // whole must not take that away.
    assert.strictEqual(before.aliases[0].self_check, 'ok', JSON.stringify(before.aliases[0]));
    assert.deepStrictEqual(summary(result.aliases), summary(before.aliases), JSON.stringify(result.aliases, null, 1));
    assert.deepStrictEqual(declarations(result.stub_dir), declarations(before.stub_dir));
    assert.deepStrictEqual(result.unpinned_externals, before.unpinned_externals);
    assert.deepStrictEqual(result.pinned_dependencies, before.pinned_dependencies);
    // And an anchor that names the package is told what is missing.
    const named = capture(repo, service, base, [
      { kind: 'symbol', alias: 'Endpoint_store_Response', symbol_name: 'Store', source_file: '@depot/records', anchor_origin: 'llm-symbol' },
    ]).aliases[0];
    assert.strictEqual(
      named.capture_failure_reason,
      `source '@depot/records' did not resolve: '@depot/records' is a package of this checkout whose entry is not on disk, and ${because}`
    );
  }
  const missing = (specifier: string): string => `its source imports '${specifier}', which does not resolve on this checkout either`;

  it('its entry re-exports a module that was never generated', () => {
    assertAsBefore(
      {
        'lib/index.ts': "export * from '../generated/schema';\nexport * from './store';\n",
        'lib/store.ts': STORE,
      },
      missing('../generated/schema')
    );
  });

  it('a file its entry reaches re-exports that module', () => {
    assertAsBefore(
      {
        'lib/index.ts': "export * from './store';\n",
        'lib/store.ts': `export * from './rows';\n${STORE}`,
        'lib/rows.ts': "export type { Row } from '../../generated/schema';\n",
      },
      missing('../../generated/schema')
    );
  });

  it('a dependency of the package that is not installed', () => {
    assertAsBefore(
      {
        'lib/index.ts': "export * from './store';\nexport type { Row } from 'rowkit';\n",
        'lib/store.ts': STORE,
      },
      missing('rowkit')
    );
  });

  // A second unbuilt package of the checkout, which `@depot/records` reads.
  const ROWS = {
    files: {
      'packages/rows/package.json': JSON.stringify({ name: '@depot/rows', version: '1.0.0', types: 'dist/index.d.ts' }),
      'packages/rows/tsconfig.json': tsconfig({ module: 'CommonJS', rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
    },
    links: { 'packages/records/node_modules/@depot/rows': 'packages/rows' },
  };
  const READS_ROWS = {
    'lib/index.ts': "export * from './store';\nexport type { Row } from '@depot/rows';\n",
    'lib/store.ts': STORE,
  };

  it('another package of the checkout it reads is not all there', () => {
    assertAsBefore(READS_ROWS, missing('../generated/schema'), {
      ...ROWS,
      files: { ...ROWS.files, 'packages/rows/lib/index.ts': "export * from '../generated/schema';\n" },
    });
  });

  // The service reads `units` at 2.0.0 and the package at 3.1.0.
  const UNITS_2 = 'node_modules/.pnpm/units@2.0.0/node_modules/units';
  const SERVICE_READS_2 = {
    files: {
      [`${UNITS_2}/package.json`]: JSON.stringify({ name: 'units', version: '2.0.0', types: 'index.d.ts' }),
      [`${UNITS_2}/index.d.ts`]: 'export interface Unit { symbol: string }\n',
    },
    links: { 'packages/api/node_modules/units': UNITS_2 },
  };
  const atAnotherVersion = (use: string): string =>
    `its source ${use} 'units', which it reads at 3.1.0, where the service reads it at 2.0.0`;

  // A declaration can leave an import unresolved and write `unknown` where a
  // type of it is used. These three uses have no such position.
  it('a dependency at another version than the service reads, exported onward from the package', () => {
    assertAsBefore(
      {
        'lib/index.ts': "export * from './store';\nexport type { Unit } from 'units';\n",
        'lib/store.ts': STORE,
      },
      atAnotherVersion('re-exports'),
      SERVICE_READS_2
    );
  });

  it('or imported and exported again by name', () => {
    assertAsBefore(
      {
        'lib/index.ts': "export * from './store';\n",
        'lib/store.ts': `import type { Unit } from 'units';\nexport type { Unit };\n${STORE}`,
      },
      atAnotherVersion('re-exports'),
      SERVICE_READS_2
    );
  });

  it('or extended by a type of the package', () => {
    assertAsBefore(
      {
        'lib/index.ts': "export * from './store';\n",
        'lib/store.ts': "import type * as units from 'units';\nexport interface Store extends units.Unit { name: string }\n",
      },
      atAnotherVersion('extends a type of'),
      SERVICE_READS_2
    );
  });

  it('a package whose entry is its source is left an installed library', () => {
    const { base, repo, service } = isolatedWorkspace(
      {
        'lib/index.ts': "export * from '../generated/schema';\nexport * from './store';\n",
        'lib/store.ts': STORE,
      },
      TALLY,
      { manifest: { types: 'lib/index.ts', main: 'lib/index.ts' } }
    );
    // Text an inference printed, naming a type the package declares.
    const anchors: CaptureAnchorRequest[] = [
      ...TALLY_ANCHORS,
      { kind: 'literal', alias: 'Endpoint_store_Text', type_text: '{ store: Store; }', source_file: 'lib/tally.ts', anchor_origin: 'deterministic-infer' },
    ];
    const before = captureAsBefore(service, base, anchors);
    const result = capture(repo, service, base, anchors);
    assert.deepStrictEqual(summary(result.aliases), summary(before.aliases), JSON.stringify(result.aliases, null, 1));
    assert.deepStrictEqual(declarations(result.stub_dir), declarations(before.stub_dir));
    assert.deepStrictEqual(result.unpinned_externals, ['@depot/records']);
  });

  // The Node runtime's types, as the service's program includes them.
  const NODE_TYPES = 'node_modules/.pnpm/@types+node@20.0.0/node_modules/@types/node';
  const EMITTER = {
    'lib/index.ts': "export * from './store';\nexport type Row = { id: string };\n",
    'lib/store.ts': "import type { EventEmitter } from 'node:events';\nexport interface Store { name: string; bus: EventEmitter }\n",
  };

  it('a module of the runtime resolves where the program holds the runtime\'s types', () => {
    const { base, repo, service } = isolatedWorkspace(EMITTER, TALLY, {
      files: {
        [`${NODE_TYPES}/package.json`]: JSON.stringify({ name: '@types/node', version: '20.0.0', types: 'index.d.ts' }),
        [`${NODE_TYPES}/index.d.ts`]: "declare module 'node:events' {\n  export class EventEmitter { on(event: string): this }\n}\n",
      },
      links: { 'packages/api/node_modules/@types/node': NODE_TYPES },
    });
    const result = capture(repo, service, base, TALLY_ANCHORS);
    assert.strictEqual(result.aliases[0].self_check, 'ok', JSON.stringify(result.aliases[0]));
    assert.ok(Object.keys(declarations(result.stub_dir)).some((file) => file.endsWith('records/lib/store.d.ts')));
  });

  it('and does not where it holds none', () => {
    assertAsBefore(EMITTER, missing('node:events'));
  });

  // The scanner names a type declared in a sibling package by the path of
  // its file, where the model saw it imported.
  const byPath = (repo: string, file: string): CaptureAnchorRequest[] => [
    TALLY_ANCHORS[0],
    { kind: 'symbol', alias: 'Endpoint_store_Response', symbol_name: 'Store', source_file: path.join(repo, 'packages/records', file), anchor_origin: 'llm-symbol' },
  ];
  const HALF_THERE = {
    'lib/index.ts': "export * from '../generated/schema';\nexport * from './store';\n",
    'lib/store.ts': STORE,
  };

  it('an anchor that names a file of the package by its path is held to the same rule', () => {
    const { base, repo, service } = isolatedWorkspace(HALF_THERE, TALLY);
    const anchors = byPath(repo, 'lib/index.ts');
    const before = captureAsBefore(service, base, anchors);
    const result = capture(repo, service, base, anchors);
    assert.match(before.aliases[1].capture_failure_reason ?? '', /^source file not in program: /);
    assert.deepStrictEqual(result.aliases, before.aliases);
    assert.deepStrictEqual(declarations(result.stub_dir), declarations(before.stub_dir));
  });

  it('and reads the file where all it reaches is there', () => {
    // The same package, entered at the one file that asks nothing of the missing module.
    const { base, repo, service } = isolatedWorkspace(HALF_THERE, TALLY);
    const result = capture(repo, service, base, byPath(repo, 'lib/store.ts'));
    assert.deepStrictEqual(summary(result.aliases), [
      ['Endpoint_tally_Response', 'ok', undefined],
      ['Endpoint_store_Response', 'ok', undefined],
    ]);
    const files = Object.keys(declarations(result.stub_dir));
    assert.ok(files.some((file) => file.endsWith('records/lib/store.d.ts')), files.join(', '));
    assert.ok(!files.some((file) => file.endsWith('records/lib/index.d.ts')), files.join(', '));
    assert.deepStrictEqual(standalone(result.stub_dir, base).members('Endpoint_store_Response'), ['name']);
  });

  it('two packages that read each other, both all there, travel together', () => {
    const { base, repo, service } = isolatedWorkspace(
      {
        'lib/index.ts': "export * from './store';\nexport type { Row } from '@depot/rows';\n",
        'lib/store.ts': STORE,
      },
      TALLY,
      {
        ...ROWS,
        files: {
          ...ROWS.files,
          'packages/rows/lib/index.ts': "import type { Store } from '@depot/records';\nexport interface Row { id: string; store: Store }\n",
        },
        links: { ...ROWS.links, 'packages/rows/node_modules/@depot/records': 'packages/records' },
      }
    );
    const result = capture(repo, service, base, TALLY_ANCHORS);
    assert.strictEqual(result.aliases[0].self_check, 'ok', JSON.stringify(result.aliases[0]));
    const files = Object.keys(declarations(result.stub_dir));
    for (const carried of ['records/lib/index.d.ts', 'records/lib/store.d.ts', 'rows/lib/index.d.ts']) {
      assert.ok(files.some((file) => file.endsWith(carried)), `${carried} not in ${files.join(', ')}`);
    }
    assert.deepStrictEqual(result.unpinned_externals, []);
    assert.deepStrictEqual(standalone(result.stub_dir, base).diagnostics, []);
  });
});

describe('carrick#1620: what a carried package depends on resolves in the stub as it did in its source', () => {
  it('a dependency only the package installs is pinned at the version the package reads', () => {
    const { base, repo, service } = isolatedWorkspace(
      {
        'lib/index.ts': "export * from './store';\n",
        'lib/store.ts': "import type { Unit } from 'units';\nexport interface Store { name: string; unit: Unit }\n",
      },
      {
        'lib/shelves.ts': "import type { Store } from '@depot/records';\nexport interface Shelf { store: Store; aisle: number }\n",
      }
    );
    const anchors: CaptureAnchorRequest[] = [
      { kind: 'symbol', alias: 'Endpoint_shelf_Response', symbol_name: 'Shelf', source_file: 'lib/shelves.ts', anchor_origin: 'llm-symbol' },
    ];
    const result = capture(repo, service, base, anchors);
    assert.deepStrictEqual(summary(result.aliases), [['Endpoint_shelf_Response', 'ok', undefined]], JSON.stringify(result.aliases, null, 1));
    assert.deepStrictEqual(result.pinned_dependencies, { units: '3.1.0' });
    assert.deepStrictEqual(result.unpinned_externals, []);
    const files = declarations(result.stub_dir);
    const store = Object.keys(files).find((file) => file.endsWith('records/lib/store.d.ts'));
    assert.ok(store, Object.keys(files).join(', '));
    assert.match(files[store], /from ['"]units['"]/);
  });
});

describe('carrick#1910: the stub states no type from a version of a package other than the one it pins', () => {
  // The package types one member of `Store` with a dependency, and declares
  // `Row` with no dependency at all.
  const RECORDS = {
    'lib/index.ts': "export * from './store';\nexport * from './rows';\n",
    'lib/store.ts': "import type { Unit } from 'units';\nexport interface Store { name: string; unit: Unit }\n",
    'lib/rows.ts': 'export interface Row { id: string; size: number }\n',
  };
  const SHELVES = {
    'lib/shelves.ts': [
      "import type { Row, Store } from '@depot/records';",
      'export interface Shelf { store: Store; aisle: number }',
      'export interface Bin { row: Row; label: string }',
      "export interface Tag { name: Store['name']; code: string }",
      'export interface Count { total: number }',
      '',
    ].join('\n'),
  };
  const symbol = (name: string): CaptureAnchorRequest => ({
    kind: 'symbol',
    alias: `Endpoint_${name.toLowerCase()}_Response`,
    symbol_name: name,
    source_file: 'lib/shelves.ts',
    anchor_origin: 'llm-symbol',
  });
  const SHELF_ANCHORS = ['Shelf', 'Bin', 'Tag', 'Count'].map(symbol);
  const unitsAt = (version: string, at: string, members: string): Record<string, string> => ({
    [`${at}/package.json`]: JSON.stringify({ name: 'units', version, types: 'index.d.ts' }),
    [`${at}/index.d.ts`]: `export interface Unit { ${members} }\n`,
  });
  const LEFT_UNRESOLVED =
    "this declaration was read with 'units' at 3.1.0, and the stub pins 2.0.0, the version the service reads; " +
    'its import of that package is left unresolved here';

  /** The member the dependency types abstains, in the one file that read it, and nothing else moves. */
  function assertLeftUnresolvedInThatFile(repo: string, service: string, base: string): void {
    const before = captureAsBefore(service, base, SHELF_ANCHORS);
    const result = capture(repo, service, base, SHELF_ANCHORS);
    // The service's version is the pin.
    assert.deepStrictEqual(result.pinned_dependencies, { units: '2.0.0' });
    assert.deepStrictEqual(result.unpinned_externals, []);
    const files = declarations(result.stub_dir);
    const carried = (name: string): string => {
      const file = Object.keys(files).find((rel) => rel.endsWith(`records/lib/${name}.d.ts`));
      assert.ok(file, `${name} not in ${Object.keys(files).join(', ')}`);
      return files[file];
    };
    // The package is carried whole; one import is gone from one file.
    assert.doesNotMatch(carried('store'), /units/);
    assert.match(carried('store'), /unit: unknown;/);
    assert.match(carried('rows'), /size: number;/);
    assert.match(carried('index'), /store/);
    assert.deepStrictEqual(standalone(result.stub_dir, base).diagnostics, []);

    const byAlias = new Map(result.aliases.map((record) => [record.alias, record]));
    const shelf = byAlias.get('Endpoint_shelf_Response')!;
    assert.strictEqual(shelf.self_check, 'decayed_internal', JSON.stringify(shelf));
    assert.deepStrictEqual(shelf.any_provenance, [
      { path: 'store.unit', kind: 'unknown', reason: 'unresolved_import', detail: LEFT_UNRESOLVED },
    ]);
    // The types beside it stand: one of the same file, one of the same package.
    for (const alias of ['Endpoint_bin_Response', 'Endpoint_tag_Response', 'Endpoint_count_Response']) {
      assert.deepStrictEqual(
        [byAlias.get(alias)!.self_check, byAlias.get(alias)!.self_check_detail],
        ['ok', undefined],
        JSON.stringify(byAlias.get(alias))
      );
    }
    const stub = standalone(result.stub_dir, base);
    assert.deepStrictEqual(stub.memberOf('Endpoint_bin_Response', 'row'), ['id', 'size']);
    assert.deepStrictEqual(stub.memberOf('Endpoint_shelf_Response', 'store'), ['name', 'unit']);
    // And no alias that read `ok` with the package unread reads worse with it carried.
    for (const record of before.aliases) {
      if (record.self_check === 'ok') assert.strictEqual(byAlias.get(record.alias)!.self_check, 'ok', record.alias);
    }
    assert.strictEqual(before.aliases.find((a) => a.alias === 'Endpoint_count_Response')!.self_check, 'ok');
  }

  it('an isolated install: the import is left unresolved in the file that read the other version', () => {
    const at2 = 'node_modules/.pnpm/units@2.0.0/node_modules/units';
    const { base, repo, service } = isolatedWorkspace(RECORDS, SHELVES, {
      files: unitsAt('2.0.0', at2, 'symbol: string'),
      links: { 'packages/api/node_modules/units': at2 },
    });
    assertLeftUnresolvedInThatFile(repo, service, base);
  });

  it('a hoisted install: the service reads the root\'s copy, the package its own', () => {
    const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1910-')));
    bases.push(base);
    const repo = path.join(base, 'repo');
    const files: Record<string, string> = {
      'package.json': JSON.stringify({ name: 'depot', private: true, workspaces: ['packages/*'] }),
      ...unitsAt('2.0.0', 'node_modules/units', 'symbol: string'),
      ...unitsAt('3.1.0', 'packages/records/node_modules/units', 'symbol: string; factor: number'),
      'packages/records/package.json': JSON.stringify({
        name: '@depot/records',
        version: '1.0.0',
        types: 'dist/index.d.ts',
        main: 'dist/index.js',
        dependencies: { units: '3.1.0' },
      }),
      'packages/records/tsconfig.json': tsconfig({ module: 'CommonJS', rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
      ...Object.fromEntries(Object.entries(RECORDS).map(([rel, text]) => [`packages/records/${rel}`, text])),
      'packages/api/package.json': JSON.stringify({ name: '@depot/api', version: '1.0.0', dependencies: { '@depot/records': '1.0.0', units: '2.0.0' } }),
      'packages/api/tsconfig.json': tsconfig({ module: 'CommonJS', rootDir: 'lib' }, { include: ['lib'] }),
      ...Object.fromEntries(Object.entries(SHELVES).map(([rel, text]) => [`packages/api/${rel}`, text])),
    };
    for (const [rel, text] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true });
      fs.writeFileSync(path.join(repo, rel), text);
    }
    fs.mkdirSync(path.join(repo, 'node_modules/@depot'), { recursive: true });
    fs.symlinkSync('../../packages/records', path.join(repo, 'node_modules/@depot/records'), 'dir');
    assertLeftUnresolvedInThatFile(repo, path.join(repo, 'packages/api'), base);
  });

  it('the same version through another install is the same reading', () => {
    // The service links its own copy of the version the package reads.
    const again = 'node_modules/.pnpm/units@3.1.0_peer/node_modules/units';
    const { base, repo, service } = isolatedWorkspace(RECORDS, SHELVES, {
      files: unitsAt('3.1.0', again, 'symbol: string; factor: number'),
      links: { 'packages/api/node_modules/units': again },
    });
    const result = capture(repo, service, base, SHELF_ANCHORS);
    assert.deepStrictEqual(summary(result.aliases), SHELF_ANCHORS.map((a) => [a.alias, 'ok', undefined]), JSON.stringify(result.aliases, null, 1));
    assert.deepStrictEqual(result.pinned_dependencies, { units: '3.1.0' });
    const files = declarations(result.stub_dir);
    assert.match(files[Object.keys(files).find((rel) => rel.endsWith('records/lib/store.d.ts'))!], /from ['"]units['"]/);
  });

  it('two packages that read one dependency at two versions, and a service that installs neither', () => {
    // `@depot/rows` reads 2.0.0 and `@depot/records` 3.1.0. The stub pins one
    // of them, and only the declaration read with that one keeps its import.
    const at2 = 'node_modules/.pnpm/units@2.0.0/node_modules/units';
    const { base, repo, service } = isolatedWorkspace(
      { ...RECORDS, 'lib/index.ts': "export * from './store';\nexport * from './rows';\nexport type { Pallet } from '@depot/rows';\n" },
      {
        'lib/shelves.ts': [
          "import type { Pallet, Store } from '@depot/records';",
          'export interface Shelf { store: Store; aisle: number }',
          'export interface Dock { pallet: Pallet; bay: number }',
          '',
        ].join('\n'),
      },
      {
        files: {
          ...unitsAt('2.0.0', at2, 'symbol: string'),
          'packages/rows/package.json': JSON.stringify({ name: '@depot/rows', version: '1.0.0', types: 'dist/index.d.ts', dependencies: { units: '2.0.0' } }),
          'packages/rows/tsconfig.json': tsconfig({ module: 'CommonJS', rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
          'packages/rows/lib/index.ts': "import type { Unit } from 'units';\nexport interface Pallet { weight: Unit; tier: number }\n",
        },
        links: { 'packages/records/node_modules/@depot/rows': 'packages/rows', 'packages/rows/node_modules/units': at2 },
      }
    );
    const result = capture(repo, service, base, [symbol('Shelf'), symbol('Dock')]);
    const pin = result.pinned_dependencies.units;
    assert.ok(pin === '2.0.0' || pin === '3.1.0', JSON.stringify(result.pinned_dependencies));
    const files = declarations(result.stub_dir);
    const text = (suffix: string): string => files[Object.keys(files).find((rel) => rel.endsWith(suffix))!];
    const [kept, repaired] = pin === '3.1.0' ? ['records/lib/store.d.ts', 'rows/lib/index.d.ts'] : ['rows/lib/index.d.ts', 'records/lib/store.d.ts'];
    assert.match(text(kept), /from ['"]units['"]/);
    assert.doesNotMatch(text(repaired), /units/);
    const [whole, abstains] = pin === '3.1.0' ? ['Endpoint_shelf_Response', 'Endpoint_dock_Response'] : ['Endpoint_dock_Response', 'Endpoint_shelf_Response'];
    const byAlias = new Map(result.aliases.map((record) => [record.alias, record]));
    assert.strictEqual(byAlias.get(whole)!.self_check, 'ok', JSON.stringify(byAlias.get(whole)));
    assert.strictEqual(byAlias.get(abstains)!.self_check, 'decayed_internal', JSON.stringify(byAlias.get(abstains)));
    assert.strictEqual(byAlias.get(abstains)!.any_provenance?.[0].reason, 'unresolved_import');
  });

  it('and where the one that lost exports the dependency onward, every type through that file is refused', () => {
    // Nothing above the service says which version it reads, so nothing could
    // be decided before the emit. The declaration read with the version the
    // stub does not pin cannot be repaired, and it is not stated either.
    const at2 = 'node_modules/.pnpm/units@2.0.0/node_modules/units';
    const { base, repo, service } = isolatedWorkspace(
      {
        'lib/index.ts': "export * from './store';\nexport type { Pallet } from '@depot/rows';\n",
        'lib/store.ts': "export type { Unit } from 'units';\nexport interface Store { name: string }\n",
      },
      {
        'lib/shelves.ts': "import type { Pallet, Store } from '@depot/records';\nexport interface Shelf { store: Store; pallet: Pallet }\n",
        'lib/count.ts': 'export interface Count { total: number }\n',
      },
      {
        files: {
          ...unitsAt('2.0.0', at2, 'symbol: string'),
          'packages/rows/package.json': JSON.stringify({ name: '@depot/rows', version: '1.0.0', types: 'dist/index.d.ts', dependencies: { units: '2.0.0' } }),
          'packages/rows/tsconfig.json': tsconfig({ module: 'CommonJS', rootDir: 'lib', outDir: 'dist' }, { include: ['lib'] }),
          'packages/rows/lib/index.ts': "export type { Unit as PalletUnit } from 'units';\nexport interface Pallet { tier: number }\n",
        },
        links: { 'packages/records/node_modules/@depot/rows': 'packages/rows', 'packages/rows/node_modules/units': at2 },
      }
    );
    const result = capture(repo, service, base, [
      symbol('Shelf'),
      { ...symbol('Count'), source_file: 'lib/count.ts' },
    ]);
    const [shelf, count] = result.aliases;
    assert.strictEqual(shelf.self_check, 'decayed_internal', JSON.stringify(shelf));
    assert.deepStrictEqual(shelf.dangling_specifiers, ['units']);
    assert.deepStrictEqual([count.self_check, count.self_check_detail], ['ok', undefined], JSON.stringify(count));
  });
});

describe('carrick#1910: a service with no package of the checkout in play captures as before', () => {
  /**
   * An isolated install, as pnpm lays one out: each package in its own
   * directory under `.pnpm`, linked into the service and into the packages
   * that depend on it. `kit` returns a type `inner` declares, and the
   * service depends on both and imports only `kit`.
   */
  function isolatedInstall(): { base: string; repo: string; service: string } {
    const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1910-')));
    bases.push(base);
    const repo = path.join(base, 'repo');
    const store = 'node_modules/.pnpm';
    const files: Record<string, string> = {
      'package.json': JSON.stringify({ name: 'depot', private: true }),
      [`${store}/inner@2.0.0/node_modules/inner/package.json`]: JSON.stringify({ name: 'inner', version: '2.0.0', types: 'index.d.ts' }),
      [`${store}/inner@2.0.0/node_modules/inner/index.d.ts`]: 'export interface Position { aisle: number; shelf: number }\n',
      [`${store}/kit@1.0.0/node_modules/kit/package.json`]: JSON.stringify({ name: 'kit', version: '1.0.0', types: 'index.d.ts', dependencies: { inner: '2.0.0' } }),
      [`${store}/kit@1.0.0/node_modules/kit/index.d.ts`]: [
        "import type { Position } from 'inner';",
        'export declare function usePosition(): Position;',
        "export type { Position } from 'inner';",
        '',
      ].join('\n'),
      'packages/api/package.json': JSON.stringify({ name: '@depot/api', version: '1.0.0', dependencies: { kit: '1.0.0', inner: '2.0.0' } }),
      'packages/api/tsconfig.json': tsconfig({ module: 'CommonJS', rootDir: 'lib' }, { include: ['lib'] }),
      'packages/api/lib/positions.ts': [
        "import { usePosition } from 'kit';",
        // The return type is inferred, so the declaration has to name it.
        'export function current() {',
        '  return usePosition();',
        '}',
        '',
      ].join('\n'),
    };
    for (const [rel, text] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true });
      fs.writeFileSync(path.join(repo, rel), text);
    }
    const link = (at: string, to: string) => {
      fs.mkdirSync(path.dirname(path.join(repo, at)), { recursive: true });
      fs.symlinkSync(path.relative(path.dirname(path.join(repo, at)), path.join(repo, to)), path.join(repo, at), 'dir');
    };
    link(`${store}/kit@1.0.0/node_modules/inner`, `${store}/inner@2.0.0/node_modules/inner`);
    link('packages/api/node_modules/kit', `${store}/kit@1.0.0/node_modules/kit`);
    link('packages/api/node_modules/inner', `${store}/inner@2.0.0/node_modules/inner`);
    return { base, repo, service: path.join(repo, 'packages/api') };
  }

  it('names a type from a package the service does not import as the compiler does', () => {
    const { base, repo, service } = isolatedInstall();
    const result = capture(repo, service, base, [
      { kind: 'handler_return', alias: 'Endpoint_current_Response', symbol_name: 'current', source_file: 'lib/positions.ts', anchor_origin: 'llm-symbol' },
    ]);
    assert.deepStrictEqual(summary(result.aliases), [['Endpoint_current_Response', 'ok', undefined]]);
    const emitted = fs.readFileSync(path.join(result.stub_dir, 'types/positions.d.ts'), 'utf8');

    // What the compiler writes for that file with no host of ours.
    const parsed = ts.getParsedCommandLineOfConfigFile(path.join(service, 'tsconfig.json'), {}, {
      ...ts.sys,
      onUnRecoverableConfigFileDiagnostic: () => {},
    })!;
    let plain = '';
    const program = ts.createProgram([path.join(service, 'lib/positions.ts')], {
      ...parsed.options,
      declaration: true,
      emitDeclarationOnly: true,
      noEmit: false,
      composite: false,
      incremental: false,
      outDir: path.join(base, 'plain'),
    });
    program.emit(undefined, (_file, text) => {
      plain = text;
    });
    assert.match(plain, /import\("inner"\)\.Position/, plain);
    assert.strictEqual(emitted, plain);
  });
});
