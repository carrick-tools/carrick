/**
 * The surface entry names an anchor's module in a form the entry resolves
 * (carrick#1911).
 *
 * The entry is a `.ts` file written inside the service, and every `symbol`
 * and `handler_return` anchor is an import type of its declaring module:
 * `import('<specifier>').Name`. The specifier was the module's path with its
 * extension removed. TypeScript resolves that form only where the module
 * format allows a bare path: under `module` `node16`..`nodenext`, a file in a
 * `"type": "module"` package must name the output file (`./parcels.js`), so
 * the import did not resolve, the module never joined the declaration emit,
 * and every such alias self-checked "dangling internal specifier". A `.mts`
 * or `.cts` module has no bare-path form in any mode, so its anchors dangled
 * everywhere.
 *
 * The output file's name resolves in every mode: `.js` for `.ts`/`.tsx`,
 * `.mjs` for `.mts`, `.cjs` for `.cts`. The entry uses it where the bare path
 * does not resolve, and keeps the bare path where it does, so a service that
 * captured before captures the same surface.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub } from '../src/capture/index.js';
import { entryRelativeSpecifier } from '../src/capture/anchors.js';
import type { CaptureAnchorRequest } from '../src/capture/api.js';

const SOURCES: Record<string, string> = {
  'lib/parcels.ts': [
    'export interface Parcel { id: string; weightKg: number }',
    'export async function trackParcel(id: string): Promise<{ parcel: Parcel; hops: number }> {',
    '  return { parcel: { id, weightKg: 2 }, hops: 3 };',
    '}',
    '',
  ].join('\n'),
  'lib/labels.mts': 'export interface Label { parcelId: string; barcode: string }\n',
  'lib/rates.cts': 'export interface Rate { zone: number; pence: number }\n',
  'lib/panel.tsx': 'export interface PanelProps { title: string; open: boolean }\n',
};

interface Layout {
  name: string;
  /** `compilerOptions` of the service's tsconfig; `undefined` writes none. */
  options?: Record<string, unknown>;
  /** The package's `"type"`. */
  type?: 'module' | 'commonjs';
  /** Whether the entry is an ES module that must name a `.ts` module's output. */
  named?: true;
}

const LAYOUTS: Layout[] = [
  // The ticket's shape: an ES module package under NodeNext.
  { name: 'nodenext, "type": "module"', options: { module: 'NodeNext' }, type: 'module', named: true },
  { name: 'node16, "type": "module"', options: { module: 'Node16' }, type: 'module', named: true },
  { name: 'nodenext, "type": "commonjs"', options: { module: 'NodeNext' }, type: 'commonjs' },
  { name: 'nodenext, no "type"', options: { module: 'NodeNext' } },
  {
    name: 'bundler, "type": "module"',
    options: { module: 'ESNext', moduleResolution: 'Bundler' },
    type: 'module',
  },
  { name: 'commonjs, node10', options: { module: 'CommonJS' } },
  { name: 'no tsconfig', type: 'module' },
];

const bases: string[] = [];
after(() => {
  for (const base of bases) fs.rmSync(base, { recursive: true, force: true });
});

function writeRepo(layout: Layout, extra: Record<string, string> = {}): { base: string; repo: string } {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1911-'));
  bases.push(base);
  const repo = path.join(base, 'repo');
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'parcel-desk', private: true, ...(layout.type ? { type: layout.type } : {}) }),
    ...SOURCES,
    ...extra,
  };
  if (layout.options) {
    files['tsconfig.json'] = JSON.stringify({
      compilerOptions: { target: 'ES2022', strict: true, skipLibCheck: true, jsx: 'preserve', ...layout.options },
      include: ['lib'],
    });
  }
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true });
    fs.writeFileSync(path.join(repo, rel), text);
  }
  return { base, repo };
}

const symbol = (alias: string, symbol_name: string, source_file: string): CaptureAnchorRequest => ({
  kind: 'symbol',
  alias,
  symbol_name,
  source_file,
  anchor_origin: 'llm-symbol',
});

/** The surface of a copy of the stub's tree, typechecked as the check does. */
function standalone(stubDir: string, base: string): { diagnostics: string[]; members: (alias: string) => string[] } {
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
  return {
    diagnostics: ts
      .getPreEmitDiagnostics(program)
      .map((d) => `${d.file ? path.relative(copy, d.file.fileName) : ''}: ${ts.flattenDiagnosticMessageText(d.messageText, ' ')}`),
    members: (alias) => {
      const decl = source.statements.find(
        (s): s is ts.TypeAliasDeclaration => ts.isTypeAliasDeclaration(s) && s.name.text === alias
      )!;
      return checker
        .getTypeAtLocation(decl)
        .getProperties()
        .map((p) => p.name)
        .sort();
    },
  };
}

describe('carrick#1911: the entry names a module by the file it compiles to', () => {
  it('keeps the bare path where the entry resolves it to the module', () => {
    const asked: string[] = [];
    const resolves = (to: string | undefined) => (specifier: string) => {
      asked.push(specifier);
      return to;
    };
    assert.strictEqual(
      entryRelativeSpecifier('/repo/lib', '/repo', 'lib/parcels.ts', resolves('/repo/lib/parcels.ts')),
      './parcels'
    );
    // Unresolved, or resolved to another file of that name: the output name.
    assert.strictEqual(entryRelativeSpecifier('/repo/lib', '/repo', 'lib/parcels.ts', resolves(undefined)), './parcels.js');
    assert.strictEqual(
      entryRelativeSpecifier('/repo/lib', '/repo', 'lib/panel.tsx', resolves('/repo/lib/panel.ts')),
      './panel.js'
    );
    assert.deepStrictEqual(asked, ['./parcels', './parcels', './panel']);
  });

  it('maps each source kind to its output extension where the bare path does not resolve', () => {
    const spec = (file: string) => entryRelativeSpecifier('/repo/lib', '/repo', file);
    assert.strictEqual(spec('lib/parcels.ts'), './parcels.js');
    assert.strictEqual(spec('lib/panel.tsx'), './panel.js');
    assert.strictEqual(spec('lib/labels.mts'), './labels.mjs');
    assert.strictEqual(spec('lib/rates.cts'), './rates.cjs');
    // A declaration file is named by the module it declares.
    assert.strictEqual(spec('lib/ambient.d.ts'), './ambient.js');
    assert.strictEqual(spec('lib/ambient.d.mts'), './ambient.mjs');
    assert.strictEqual(spec('lib/ambient.d.cts'), './ambient.cjs');
    // A script the service keeps as JavaScript already carries its name.
    assert.strictEqual(spec('lib/legacy.js'), './legacy.js');
    assert.strictEqual(spec('lib/legacy.jsx'), './legacy.js');
    assert.strictEqual(spec('lib/legacy.mjs'), './legacy.mjs');
    assert.strictEqual(spec('lib/legacy.cjs'), './legacy.cjs');
    // From another directory, and for a name that has dots of its own.
    assert.strictEqual(entryRelativeSpecifier('/repo/lib', '/repo', 'src/v2/parcel.schema.ts'), '../src/v2/parcel.schema.js');
    assert.strictEqual(entryRelativeSpecifier('/repo', '/repo', 'lib/parcels.ts'), './lib/parcels.js');
  });

  for (const layout of LAYOUTS) {
    it(`resolves a symbol and a handler anchor on a sibling file: ${layout.name}`, () => {
      const { base, repo } = writeRepo(layout);
      // The compiler adds an imported `.tsx` module to a program only when
      // `jsx` is set, which the layout with no tsconfig does not do.
      const tsx = layout.options !== undefined;
      const anchors: CaptureAnchorRequest[] = [
        symbol('Endpoint_parcel_Response', 'Parcel', 'lib/parcels.ts'),
        {
          kind: 'handler_return',
          alias: 'Endpoint_track_Response',
          symbol_name: 'trackParcel',
          source_file: 'lib/parcels.ts',
          anchor_origin: 'llm-symbol',
        },
        ...(tsx ? [symbol('Endpoint_panel_Request', 'PanelProps', 'lib/panel.tsx')] : []),
        // A literal whose text is a sibling symbol anchor's name, and one whose
        // name the file it was printed in declares: both are import types of
        // the declaring module.
        { kind: 'literal', alias: 'Endpoint_sibling_Response', type_text: 'Parcel', anchor_origin: 'deterministic-infer' },
        {
          kind: 'literal',
          alias: 'Endpoint_printed_Response',
          type_text: '{ first: Parcel; total: number }',
          source_file: 'lib/parcels.ts',
          anchor_origin: 'deterministic-infer',
        },
        symbol('Endpoint_label_Response', 'Label', 'lib/labels.mts'),
        symbol('Endpoint_rate_Response', 'Rate', 'lib/rates.cts'),
      ];
      const result = captureStub({
        repoRoot: repo,
        serviceName: 'parcel-desk',
        outDir: path.join(base, 'stub'),
        anchors,
      });
      assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
      assert.deepStrictEqual(
        result.aliases.map((a) => [a.alias, a.serialization, a.self_check, a.self_check_detail]),
        anchors.map((a) => [a.alias, a.kind === 'literal' ? 'structural_fallback' : 'emitted', 'ok', undefined]),
        JSON.stringify(result.aliases, null, 1)
      );
      const expected = [
        'types/lib/labels.d.mts',
        'types/lib/parcels.d.ts',
        'types/lib/rates.d.cts',
        'types/surface.d.ts',
        ...(tsx ? ['types/lib/panel.d.ts'] : []),
      ].sort();
      assert.deepStrictEqual(result.emitted_files, expected);

      // The form each module is named in: the bare path wherever the entry
      // resolves it, as before, and the output name where it does not.
      const surface = fs.readFileSync(path.join(result.stub_dir, 'types/surface.d.ts'), 'utf8');
      const line = (alias: string) => surface.split('\n').find((text) => text.includes(`type ${alias} `));
      assert.strictEqual(
        line('Endpoint_parcel_Response'),
        `export type Endpoint_parcel_Response = import('./lib/parcels${layout.named ? '.js' : ''}').Parcel;`
      );
      assert.strictEqual(line('Endpoint_label_Response'), "export type Endpoint_label_Response = import('./lib/labels.mjs').Label;");
      assert.strictEqual(line('Endpoint_rate_Response'), "export type Endpoint_rate_Response = import('./lib/rates.cjs').Rate;");

      const stub = standalone(result.stub_dir, base);
      assert.deepStrictEqual(stub.diagnostics, []);
      assert.deepStrictEqual(stub.members('Endpoint_parcel_Response'), ['id', 'weightKg']);
      assert.deepStrictEqual(stub.members('Endpoint_track_Response'), ['hops', 'parcel']);
      if (tsx) assert.deepStrictEqual(stub.members('Endpoint_panel_Request'), ['open', 'title']);
      assert.deepStrictEqual(stub.members('Endpoint_sibling_Response'), ['id', 'weightKg']);
      assert.deepStrictEqual(stub.members('Endpoint_printed_Response'), ['first', 'total']);
      assert.deepStrictEqual(stub.members('Endpoint_label_Response'), ['barcode', 'parcelId']);
      assert.deepStrictEqual(stub.members('Endpoint_rate_Response'), ['pence', 'zone']);
    });
  }

  it('resolves from an entry inside rootDir to a module in a nested directory', () => {
    const { base, repo } = writeRepo(
      { name: 'rootDir', options: { module: 'NodeNext', rootDir: 'lib' }, type: 'module' },
      { 'lib/depot/index.ts': 'export interface Depot { code: string; bays: number }\n' }
    );
    const result = captureStub({
      repoRoot: repo,
      serviceName: 'parcel-desk',
      outDir: path.join(base, 'stub'),
      anchors: [symbol('Endpoint_depot_Response', 'Depot', 'lib/depot/index.ts')],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    assert.deepStrictEqual(
      result.aliases.map((a) => [a.alias, a.self_check, a.self_check_detail]),
      [['Endpoint_depot_Response', 'ok', undefined]]
    );
    assert.deepStrictEqual(standalone(result.stub_dir, base).members('Endpoint_depot_Response'), ['bays', 'code']);
  });

  // The module resolves and is emitted; what it imports is missing, in a
  // position the declaration repair cannot rewrite (a heritage clause). The
  // alias's closure has to reach the module's declaration to say so: a
  // closure that stopped at the surface would read the alias clean.
  for (const [kind, file, missing] of [
    ['.mts', 'lib/manifest.mts', './absent.mjs'],
    ['.cts', 'lib/manifest.cts', './absent.cjs'],
    ['.ts', 'lib/manifest.ts', './absent.js'],
  ] as const) {
    it(`blames a ${kind} module's own dangling import on the alias that reaches it`, () => {
      const { base, repo } = writeRepo(
        { name: 'dangling', options: { module: 'NodeNext' }, type: 'module' },
        {
          [file]: [
            `import type { Sealed } from '${missing}';`,
            'export interface Manifest extends Sealed { parcels: number }',
            '',
          ].join('\n'),
        }
      );
      const result = captureStub({
        repoRoot: repo,
        serviceName: 'parcel-desk',
        outDir: path.join(base, 'stub'),
        anchors: [
          symbol('Endpoint_manifest_Response', 'Manifest', file),
          symbol('Endpoint_parcel_Response', 'Parcel', 'lib/parcels.ts'),
        ],
      });
      assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
      const byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
      const manifest = byAlias.get('Endpoint_manifest_Response')!;
      assert.strictEqual(manifest.self_check, 'decayed_internal', JSON.stringify(manifest));
      assert.deepStrictEqual(manifest.dangling_specifiers, [missing]);
      // The alias beside it is untouched.
      assert.strictEqual(byAlias.get('Endpoint_parcel_Response')!.self_check, 'ok');
    });
  }
});
