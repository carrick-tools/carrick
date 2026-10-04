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
