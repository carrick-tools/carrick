/**
 * Partial-emit recovery reads an alias's specifiers the way the tree
 * placement and the specifier rewrite do (carrick#1773).
 *
 * When one file's declaration emit is skipped, the capture keeps the emitted
 * subset and demotes the aliases that name a module the tree lacks. That test
 * joined every specifier onto the surface's directory inside the tree, which
 * is only right for a relative specifier that stays under rootDir. It read
 * these as missing, and demoted the alias with "declaration emit was skipped
 * for module ...", although nothing they name was skipped:
 *
 *  - an absolute `import("/checkout/src/m")` path to a module under rootDir,
 *    which the specifier rewrite maps onto the tree file;
 *  - a module outside rootDir, by an absolute path or by `../`, whose
 *    declaration the placement puts under `__outside__/`;
 *  - an absolute path into an installed package, which the rewrite turns into
 *    the package's bare specifier and a pin. No emit writes that module.
 *
 * So on a service with one unnameable export, every literal anchor whose text
 * the inferrer printed with a path, and every literal that names a package's
 * type, read `unknown`.
 *
 * An alias that names the skipped module is still demoted, by whichever kind
 * of specifier names it, and so is one whose specifier reaches neither a tree
 * module nor an installed package. A RELATIVE path into a package stays
 * demoted too: no rewrite maps it, so kept it would ship a path out of the
 * stub (carrick#1857).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import { surfaceModuleInTree } from '../src/capture/outside-root.js';
import type {
  CaptureAliasRecord,
  CaptureAnchorRequest,
  CaptureStubResult,
} from '../src/capture/api.js';

/** An ambient module whose `export =` hides the type its default export
 * needs: the importing file's `export default` cannot be named (TS4023) and
 * that one file's declaration is skipped. */
const UNNAMEABLE_STUB = [
  'declare module "tinyserver" {',
  '  interface Server { get(path: string, handler: () => void): this }',
  '  function create(): Server;',
  '  export = create;',
  '}',
  '',
].join('\n');

const SKIPPED_ROUTES = [
  'import create from "tinyserver";',
  'const server = create();',
  'export interface RouteReply { id: string; message: string }',
  'export default server;',
  '',
].join('\n');

/** The same module with nothing unnameable: the emit is whole. */
const WHOLE_ROUTES = 'export interface RouteReply { id: string; message: string }\n';

function write(root: string, rel: string, text: string): void {
  const file = path.join(root, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

function stubFiles(dir: string): string[] {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? stubFiles(path.join(dir, entry.name)) : [path.join(dir, entry.name)]
  );
}

/** The surface statement of one alias, on one line. */
function surfaceLine(surface: string, alias: string): string {
  const start = surface.indexOf(`export type ${alias} =`);
  assert.ok(start >= 0, `no surface line for ${alias}:\n${surface}`);
  const next = surface.indexOf('\nexport type ', start + 1);
  return surface.slice(start, next < 0 ? undefined : next).replace(/\s+/g, ' ').trim();
}

interface Capture {
  result: CaptureStubResult;
  records: Map<string, CaptureAliasRecord>;
  surface: string;
}

/**
 * A service at `apps/svc` in a workspace: its own modules, a sibling
 * package's source outside its root, a package installed in its
 * `node_modules`, and one in a runtime's npm cache outside the checkout.
 */
function captureService(scratch: string, name: string, routes: string): Capture {
  const workspace = path.join(scratch, name);
  const repoRoot = path.join(workspace, 'apps', 'svc');
  const cacheRoot = path.join(workspace, 'runtime-cache');

  write(repoRoot, 'tsconfig.json', JSON.stringify({
    compilerOptions: {
      target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler',
      strict: true, esModuleInterop: true, skipLibCheck: true,
    },
    include: ['src'],
  }));
  write(repoRoot, 'package.json', JSON.stringify({
    name: 'svc', version: '0.0.0', dependencies: { ledgerkit: '1.2.3' },
  }));
  write(repoRoot, 'src/stubs.d.ts', UNNAMEABLE_STUB);
  write(repoRoot, 'src/http/routes.ts', routes);
  write(repoRoot, 'src/events.ts', [
    "import type { Money } from 'ledgerkit';",
    "import type { Tag } from '../../../packages/shared/src/tag';",
    'export interface OrderPlaced { id: string; total: Money; tag: Tag }',
    '',
  ].join('\n'));
  write(repoRoot, 'src/models/index.ts', 'export interface Customer { email: string }\n');
  write(workspace, 'packages/shared/src/tag.ts', 'export interface Tag { label: string }\n');
  write(repoRoot, 'node_modules/ledgerkit/package.json', JSON.stringify({
    name: 'ledgerkit', version: '1.2.3', types: './index.d.ts',
  }));
  write(repoRoot, 'node_modules/ledgerkit/index.d.ts',
    'export interface Money { cents: number; currency: string }\n');
  write(cacheRoot, 'npm/registry.example.org/cachedkit/3.1.0/package.json', JSON.stringify({
    name: 'cachedkit', version: '3.1.0', exports: { '.': { types: './dist/types.d.ts' } },
  }));
  write(cacheRoot, 'npm/registry.example.org/cachedkit/3.1.0/dist/types.d.ts',
    'export interface Cached { hits: number }\n');

  const literal = (alias: string, type_text: string, source_file?: string): CaptureAnchorRequest => ({
    kind: 'literal', alias, type_text, anchor_origin: 'deterministic-infer',
    ...(source_file ? { source_file } : {}),
  });
  const symbol = (alias: string, symbol_name: string, source_file: string): CaptureAnchorRequest => ({
    kind: 'symbol', alias, symbol_name, source_file, anchor_origin: 'llm-symbol',
  });
  const outside = path.join(workspace, 'packages/shared/src/tag');

  const result = captureStub({
    repoRoot,
    serviceName: name,
    outDir: path.join(scratch, `${name}-stub`),
    anchors: [
      // Named in the tree or in a package: nothing these name was skipped.
      literal('Kept_AbsInRoot', `{ event: import("${repoRoot}/src/events").OrderPlaced }`),
      literal('Kept_AbsIndex', `{ customer: import("${repoRoot}/src/models").Customer }`),
      literal('Kept_AbsOutside', `{ tag: import("${outside}").Tag }`),
      literal('Kept_RelOutside', '{ tag: import("../../packages/shared/src/tag").Tag }'),
      symbol('Kept_SymbolOutside', 'Tag', '../../packages/shared/src/tag.ts'),
      literal('Kept_AbsPackage', `{ total: import("${repoRoot}/node_modules/ledgerkit/index.d.ts").Money }`),
      literal('Kept_NamedPackage', '{ total: Money }', 'src/events.ts'),
      literal('Kept_AbsCache',
        `{ cached: import("${cacheRoot}/npm/registry.example.org/cachedkit/3.1.0/dist/types").Cached }`),
      // Named in the module whose declaration was skipped.
      symbol('Demoted_Symbol', 'RouteReply', 'src/http/routes.ts'),
      literal('Demoted_AbsSkipped', `{ reply: import("${repoRoot}/src/http/routes").RouteReply }`),
      literal('Demoted_RelSkipped', '{ reply: import("./src/http/routes").RouteReply }'),
      literal('Demoted_Mixed',
        `{ event: import("${repoRoot}/src/events").OrderPlaced; reply: import("${repoRoot}/src/http/routes").RouteReply }`),
      // Named nowhere: not in the tree, not in a package.
      literal('Demoted_AbsNowhere', `{ gone: import("${repoRoot}/src/gone").Gone }`),
      // A package by a relative path: the rewrite maps only an absolute one.
      literal('Demoted_RelPackage', '{ total: import("./node_modules/ledgerkit/index").Money }'),
    ],
  });
  assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
  return {
    result,
    records: new Map(result.aliases.map((record) => [record.alias, record])),
    surface: fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8'),
  };
}

const KEPT = [
  'Kept_AbsInRoot',
  'Kept_AbsIndex',
  'Kept_AbsOutside',
  'Kept_RelOutside',
  'Kept_SymbolOutside',
  'Kept_AbsPackage',
  'Kept_NamedPackage',
  'Kept_AbsCache',
];

describe('partial emit keeps an alias whose specifier names a module that was not skipped (#1773)', () => {
  let scratch: string;
  let partial: Capture;
  let whole: Capture;

  before(() => {
    // Real path: the anchors name modules by absolute path.
    scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-partial-spec-')));
    partial = captureService(scratch, 'partial-svc', SKIPPED_ROUTES);
    whole = captureService(scratch, 'whole-svc', WHOLE_ROUTES);
  });

  after(() => {
    fs.rmSync(scratch, { recursive: true, force: true });
  });

  it('the fixture is a partial emit: one module skipped, the rest of the tree kept', () => {
    assert.ok(
      partial.result.errors.some((e) => /declaration emit was partial/.test(e)),
      JSON.stringify(partial.result.errors)
    );
    const files = partial.result.emitted_files;
    assert.ok(!files.includes('types/src/http/routes.d.ts'), JSON.stringify(files));
    for (const held of [
      'types/src/events.d.ts',
      'types/src/models/index.d.ts',
      'types/__outside__/packages/shared/src/tag.d.ts',
    ]) {
      assert.ok(files.includes(held), `${held} missing from ${JSON.stringify(files)}`);
    }
    // The control emits everything.
    assert.ok(!whole.result.errors.some((e) => /declaration emit was partial/.test(e)));
    assert.ok(whole.result.emitted_files.includes('types/src/http/routes.d.ts'));
  });

  it('an absolute path to a module under rootDir is kept, as a tree-relative import', () => {
    const record = partial.records.get('Kept_AbsInRoot')!;
    assert.strictEqual(record.capture_failure_reason, undefined);
    assert.strictEqual(record.self_check, 'ok', record.self_check_detail);
    assert.strictEqual(
      surfaceLine(partial.surface, 'Kept_AbsInRoot'),
      'export type Kept_AbsInRoot = { event: import("./src/events").OrderPlaced; };'
    );
  });

  it('an absolute path to a directory module is kept through its index', () => {
    const record = partial.records.get('Kept_AbsIndex')!;
    assert.strictEqual(record.capture_failure_reason, undefined);
    assert.strictEqual(record.self_check, 'ok', record.self_check_detail);
    assert.match(surfaceLine(partial.surface, 'Kept_AbsIndex'), /import\("\.\/src\/models(\/index)?"\)\.Customer/);
  });

  it('a module outside rootDir is kept, by absolute path and by ../, under __outside__', () => {
    for (const alias of ['Kept_AbsOutside', 'Kept_RelOutside', 'Kept_SymbolOutside']) {
      const record = partial.records.get(alias)!;
      assert.strictEqual(record.capture_failure_reason, undefined, alias);
      assert.strictEqual(record.self_check, 'ok', `${alias}: ${record.self_check_detail}`);
      assert.match(
        surfaceLine(partial.surface, alias),
        /import\(["']\.\/__outside__\/packages\/shared\/src\/tag["']\)\.Tag/,
        alias
      );
    }
    assert.strictEqual(partial.records.get('Kept_SymbolOutside')!.serialization, 'emitted');
  });

  it('an absolute path into an installed package is kept, as the bare specifier and a pin', () => {
    for (const alias of ['Kept_AbsPackage', 'Kept_NamedPackage']) {
      const record = partial.records.get(alias)!;
      assert.strictEqual(record.capture_failure_reason, undefined, alias);
      assert.strictEqual(record.self_check, 'ok', `${alias}: ${record.self_check_detail}`);
      assert.strictEqual(
        surfaceLine(partial.surface, alias),
        `export type ${alias} = { total: import("ledgerkit").Money; };`
      );
    }
    const cache = partial.records.get('Kept_AbsCache')!;
    assert.strictEqual(cache.capture_failure_reason, undefined);
    assert.strictEqual(
      surfaceLine(partial.surface, 'Kept_AbsCache'),
      'export type Kept_AbsCache = { cached: import("cachedkit").Cached; };'
    );
    assert.strictEqual(partial.result.pinned_dependencies.ledgerkit, '1.2.3');
    assert.strictEqual(partial.result.pinned_dependencies.cachedkit, '3.1.0');
  });

  it('every kept alias reads on the partial emit exactly as on a whole one', () => {
    for (const alias of KEPT) {
      assert.strictEqual(
        surfaceLine(partial.surface, alias),
        surfaceLine(whole.surface, alias),
        alias
      );
      assert.deepStrictEqual(partial.records.get(alias), whole.records.get(alias), alias);
    }
  });

  it('an alias that names the skipped module is still demoted, by any kind of specifier', () => {
    for (const alias of ['Demoted_Symbol', 'Demoted_AbsSkipped', 'Demoted_RelSkipped', 'Demoted_Mixed']) {
      const record = partial.records.get(alias)!;
      assert.strictEqual(record.serialization, 'structural_fallback', alias);
      assert.strictEqual(record.self_check, 'decayed_internal', alias);
      assert.match(
        record.capture_failure_reason ?? '',
        /declaration emit was skipped for module '[^']*src\/http\/routes'/,
        alias
      );
      assert.strictEqual(surfaceLine(partial.surface, alias), `export type ${alias} = unknown;`);
    }
    // On the whole emit the same anchors resolve.
    for (const alias of ['Demoted_Symbol', 'Demoted_AbsSkipped', 'Demoted_RelSkipped', 'Demoted_Mixed']) {
      const record = whole.records.get(alias)!;
      assert.strictEqual(record.capture_failure_reason, undefined, alias);
      assert.strictEqual(record.self_check, 'ok', `${alias}: ${record.self_check_detail}`);
    }
  });

  it('a path that reaches neither a tree module nor a package is still demoted', () => {
    const record = partial.records.get('Demoted_AbsNowhere')!;
    assert.strictEqual(record.self_check, 'decayed_internal');
    assert.match(record.capture_failure_reason ?? '', /declaration emit was skipped for module/);
    assert.strictEqual(
      surfaceLine(partial.surface, 'Demoted_AbsNowhere'),
      'export type Demoted_AbsNowhere = unknown;'
    );
  });

  it('a relative path into a package is still demoted: no rewrite maps it (#1857)', () => {
    const record = partial.records.get('Demoted_RelPackage')!;
    assert.strictEqual(record.self_check, 'decayed_internal');
    assert.match(record.capture_failure_reason ?? '', /declaration emit was skipped for module/);
    assert.strictEqual(
      surfaceLine(partial.surface, 'Demoted_RelPackage'),
      'export type Demoted_RelPackage = unknown;'
    );
    // What keeping it would ship: on the whole emit the path stays as written,
    // and the stub cannot resolve it.
    assert.match(surfaceLine(whole.surface, 'Demoted_RelPackage'), /import\("\.\/node_modules\/ledgerkit\/index"\)/);
    assert.strictEqual(whole.records.get('Demoted_RelPackage')!.self_check, 'decayed_internal');
  });

  it('no declaration in the tree holds a checkout path', () => {
    const typesDir = path.join(partial.result.stub_dir, 'types');
    for (const file of stubFiles(typesDir)) {
      const text = fs.readFileSync(file, 'utf8');
      assert.ok(!text.includes(scratch), `${path.relative(typesDir, file)} holds ${scratch}:\n${text}`);
    }
  });
});

describe('check v2: an alias kept on a partial emit is judged on its type (#1773)', () => {
  let scratch: string;
  let producer: CaptureStubResult;
  let consumer: CaptureStubResult;

  before(() => {
    scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-partial-spec-check-')));
    // No package here: the check installs what a stub pins.
    const repoRoot = path.join(scratch, 'producer');
    write(repoRoot, 'tsconfig.json', JSON.stringify({
      compilerOptions: {
        target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler',
        strict: true, esModuleInterop: true, skipLibCheck: true,
      },
      include: ['src'],
    }));
    write(repoRoot, 'src/stubs.d.ts', UNNAMEABLE_STUB);
    write(repoRoot, 'src/http/routes.ts', SKIPPED_ROUTES);
    write(repoRoot, 'src/events.ts', 'export interface OrderPlaced { id: string; total: number }\n');
    producer = captureStub({
      repoRoot,
      serviceName: 'partial-producer',
      outDir: path.join(scratch, 'producer-stub'),
      anchors: [
        {
          kind: 'literal',
          alias: 'P_Event',
          type_text: `{ event: import("${repoRoot}/src/events").OrderPlaced }`,
          anchor_origin: 'deterministic-infer',
        },
        // Brings the module with the unnameable export into the program.
        {
          kind: 'symbol',
          alias: 'P_Reply',
          symbol_name: 'RouteReply',
          source_file: 'src/http/routes.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.ok(producer.success, JSON.stringify(producer.errors));
    assert.ok(
      producer.errors.some((e) => /declaration emit was partial/.test(e)),
      JSON.stringify(producer.errors)
    );

    const consumerRoot = path.join(scratch, 'consumer');
    fs.mkdirSync(consumerRoot, { recursive: true });
    consumer = captureStub({
      repoRoot: consumerRoot,
      serviceName: 'partial-consumer',
      outDir: path.join(scratch, 'consumer-stub'),
      anchors: [
        {
          kind: 'literal',
          alias: 'C_Agrees',
          type_text: '{ event: { id: string; total: number } }',
          anchor_origin: 'deterministic-infer',
        },
        {
          kind: 'literal',
          alias: 'C_Differs',
          type_text: '{ event: { id: string; total: string } }',
          anchor_origin: 'deterministic-infer',
        },
      ],
    });
    assert.ok(consumer.success, JSON.stringify(consumer.errors));
  });

  after(() => {
    fs.rmSync(scratch, { recursive: true, force: true });
  });

  it('it reads compatible against a matching consumer and incompatible against a differing one', async () => {
    const pair = (pair_key: string, alias: string) => ({
      pair_key,
      protocol: 'http' as const,
      type_kind: 'response' as const,
      producer: { service_name: 'partial-producer', alias: 'P_Event' },
      consumer: { service_name: 'partial-consumer', alias },
    });
    const result = await runCheck({
      stubs: [
        { service_name: 'partial-producer', stub_dir: producer.stub_dir },
        { service_name: 'partial-consumer', stub_dir: consumer.stub_dir },
      ],
      pairs: [pair('agrees', 'C_Agrees'), pair('differs', 'C_Differs')],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const byKey = new Map(result.verdicts.map((verdict) => [verdict.pair_key, verdict]));
    assert.strictEqual(byKey.get('agrees')!.bucket, 'compatible', JSON.stringify(byKey.get('agrees')));
    assert.strictEqual(byKey.get('differs')!.bucket, 'incompatible', JSON.stringify(byKey.get('differs')));
  });
});

describe('surfaceModuleInTree reads a specifier from where the surface entry was written (#1773)', () => {
  const staging = '/scratch/staging';
  const entryDir = '/checkout/apps/svc';
  const surfaceDeclaration = '__surface__.d.ts';
  const test = (emitted: string[], declarationSources: string[] = []) =>
    surfaceModuleInTree({ emitted, declarationSources, staging, entryDir, surfaceDeclaration });

  it('an entry at rootDir: relative and absolute paths, extensions, and a directory index', () => {
    const inTree = test([
      `${staging}/${surfaceDeclaration}`,
      `${staging}/src/events.d.ts`,
      `${staging}/src/models/index.d.ts`,
      `${staging}/src/esm/wire.d.mts`,
    ]);
    for (const spec of [
      './src/events',
      './src/events.js',
      `${entryDir}/src/events`,
      `${entryDir}/src/events.ts`,
      './src/models',
      './src/models/index',
      `${entryDir}/src/models`,
      './src/esm/wire.mjs',
    ]) {
      assert.strictEqual(inTree(spec), true, spec);
    }
    for (const spec of ['./src/http/routes', `${entryDir}/src/http/routes`, '/src/events', '../svc-other/src/events']) {
      assert.strictEqual(inTree(spec), false, spec);
    }
  });

  it('a dotted module name keeps its last part', () => {
    const inTree = test([`${staging}/${surfaceDeclaration}`, `${staging}/src/orders.types.d.ts`]);
    assert.strictEqual(inTree('./src/orders.types'), true);
    assert.strictEqual(inTree(`${entryDir}/src/orders.types`), true);
    assert.strictEqual(inTree('./src/orders'), false);
  });

  it('a module outside rootDir, emitted at its own path', () => {
    const inTree = test([
      `${staging}/${surfaceDeclaration}`,
      '/checkout/packages/shared/src/tag.d.ts',
    ]);
    assert.strictEqual(inTree('../../packages/shared/src/tag'), true);
    assert.strictEqual(inTree('/checkout/packages/shared/src/tag'), true);
    assert.strictEqual(inTree('../../packages/shared/src/other'), false);
  });

  it('an entry in a directory beneath rootDir reads its specifiers from there', () => {
    const inTree = test([
      `${staging}/.cache/run/${surfaceDeclaration}`,
      `${staging}/src/events.d.ts`,
    ]);
    assert.strictEqual(inTree('../../src/events'), true);
    assert.strictEqual(inTree(`${entryDir}/src/events`), true);
    // Read from rootDir instead, this would name the module.
    assert.strictEqual(inTree('./src/events'), false);
  });

  it('an entry outside rootDir reads its specifiers from its own directory', () => {
    const inTree = test([
      `/checkout/.cache/run/${surfaceDeclaration}`,
      `${staging}/src/events.d.ts`,
    ]);
    assert.strictEqual(inTree('../../apps/svc/src/events'), true);
    assert.strictEqual(inTree(`${entryDir}/src/events`), true);
    assert.strictEqual(inTree('./src/events'), false);
  });

  it('a declaration source shipped verbatim is in the tree', () => {
    const inTree = test([`${staging}/${surfaceDeclaration}`], ['src/stubs.d.ts']);
    assert.strictEqual(inTree('./src/stubs'), true);
    assert.strictEqual(inTree(`${entryDir}/src/stubs.d.ts`), true);
  });
});
