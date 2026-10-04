/**
 * carrick#1855: a literal's bare name that an installed package declares is
 * imported from that package.
 *
 * The v1 inferrer prints some types with no enclosing declaration, so a
 * member typed by a package's type comes back as `balance: Decimal` although
 * the route's file never imports `Decimal`. The inference records what the
 * name meant (`printed_names`, #1836): the module that declares it and the
 * export path there. #1836 imported such a name only when that module is
 * inside the repo. A package's module was left alone, the surface named the
 * type bare, and the member read `any`.
 *
 * The capture now imports it under the gate a name the SOURCE imports from a
 * package already passes (#1789): the declaring file lies inside an installed
 * package, and the bare specifier that package gives the file resolves from
 * the surface entry to that same file. The entry names the module by its file
 * path; the specifier rewrite turns the path into the bare specifier and pins
 * the installed version.
 *
 * Left as written, each for its own reason:
 *  - a name printed for two declarations: the text no longer says which;
 *  - a file the package's `exports` do not reach, whether or not the entry
 *    re-exports the name: no specifier resolves to that file;
 *  - a package the entry cannot resolve by name (nested under another's
 *    `node_modules`): the service does not reach that copy;
 *  - a workspace package linked into `node_modules` from outside the service
 *    root: its files are not an installed copy, and nothing pins it (#1620);
 *  - a package that is not installed: there is no declaration to import.
 *
 * The test reads the stub with the compiler, against the repo's installed
 * packages, as well as reading the capture's own record.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub } from '../src/capture/index.js';
import type {
  CaptureAliasRecord,
  CaptureAnchorRequest,
  CaptureStubResult,
  PrintedName,
} from '../src/capture/api.js';

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

/**
 * A service whose route file reaches four packages through a module it
 * imports, and binds none of their names itself.
 *
 *  - `ledgerkit`: an entry, one exported subpath, and two files its `exports`
 *    hide (one the entry re-exports a type from, one it does not);
 *  - `decimalkit`: installed beside it, and only `ledgerkit` depends on it;
 *  - `hiddenkit`: installed under `ledgerkit`'s own `node_modules`;
 *  - `alpha` and `beta`: both export a type named `Token`;
 *  - `@ws/shared`: a workspace package outside the service root, linked in.
 */
function writeService(repoRoot: string, routes: string): void {
  write(repoRoot, 'tsconfig.json', JSON.stringify({
    compilerOptions: {
      target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler',
      strict: true, esModuleInterop: true, skipLibCheck: true,
    },
    include: ['src'],
  }));
  write(repoRoot, 'package.json', JSON.stringify({
    name: 'svc', version: '0.0.0',
    dependencies: { ledgerkit: '2.1.0', alpha: '1.0.0', beta: '1.0.0', ghostkit: '9.9.9' },
  }));

  const ledgerkit = 'node_modules/ledgerkit';
  write(repoRoot, `${ledgerkit}/package.json`, JSON.stringify({
    name: 'ledgerkit', version: '2.1.0',
    exports: {
      '.': { types: './index.d.ts', default: './index.js' },
      './money': { types: './money.d.ts', default: './money.js' },
    },
  }));
  write(repoRoot, `${ledgerkit}/index.d.ts`, [
    "import type { Decimal } from 'decimalkit';",
    "import type { Hidden } from 'hiddenkit';",
    "import type { Money } from './money';",
    "import type { AuditTrail } from './internal/audit';",
    "export type { Rate } from './internal/rate';",
    'export interface Account { id: string; balance: Decimal; held: Money; audit: AuditTrail; hidden: Hidden }',
    'export declare namespace Ledger { type Entry = { ref: string; posted: boolean } }',
    '',
  ].join('\n'));
  write(repoRoot, `${ledgerkit}/money.d.ts`, 'export interface Money { cents: number; currency: string }\n');
  write(repoRoot, `${ledgerkit}/internal/audit.d.ts`, 'export interface AuditTrail { by: string }\n');
  write(repoRoot, `${ledgerkit}/internal/rate.d.ts`, 'export interface Rate { percent: number }\n');
  write(repoRoot, `${ledgerkit}/node_modules/hiddenkit/package.json`,
    JSON.stringify({ name: 'hiddenkit', version: '0.3.0', types: './index.d.ts' }));
  write(repoRoot, `${ledgerkit}/node_modules/hiddenkit/index.d.ts`, 'export interface Hidden { secret: string }\n');

  write(repoRoot, 'node_modules/decimalkit/package.json',
    JSON.stringify({ name: 'decimalkit', version: '10.4.3', types: './index.d.ts' }));
  write(repoRoot, 'node_modules/decimalkit/index.d.ts', [
    'export declare class Decimal {',
    '  constructor(value: string | number);',
    '  readonly digits: number[];',
    '  plus(other: Decimal): Decimal;',
    '  toJSON(): string;',
    '}',
    '',
  ].join('\n'));

  for (const [name, member] of [['alpha', 'a: string'], ['beta', 'b: number']]) {
    write(repoRoot, `node_modules/${name}/package.json`,
      JSON.stringify({ name, version: '1.0.0', types: './index.d.ts' }));
    write(repoRoot, `node_modules/${name}/index.d.ts`, `export type Token = { ${member} };\n`);
  }

  const shared = `${repoRoot}-shared`;
  write(shared, 'package.json', JSON.stringify({ name: '@ws/shared', version: '0.0.0', types: './index.ts' }));
  write(shared, 'index.ts', 'export interface Shared { tag: string }\n');
  fs.mkdirSync(path.join(repoRoot, 'node_modules', '@ws'), { recursive: true });
  fs.symlinkSync(shared, path.join(repoRoot, 'node_modules', '@ws', 'shared'), 'dir');

  write(repoRoot, 'src/model.ts', "export type Status = 'open' | 'closed';\n");
  // The packages reach the program the way a route's types reach it: through
  // a module the route's file imports.
  write(repoRoot, 'src/repo.ts', [
    "import type { Account } from 'ledgerkit';",
    "import type { Token as AlphaToken } from 'alpha';",
    "import type { Token as BetaToken } from 'beta';",
    "import type { Shared } from '@ws/shared';",
    "import type { Status } from './model';",
    'export interface Row { account: Account; alpha: AlphaToken; beta: BetaToken; shared: Shared; status: Status }',
    '',
  ].join('\n'));
  write(repoRoot, 'src/router.ts', [
    "import type * as Repo from './repo';",
    'export function handler(_row?: Repo.Row): void {}',
    '',
  ].join('\n'));
  write(repoRoot, 'src/http/routes.ts', routes);
}

interface Capture {
  repoRoot: string;
  result: CaptureStubResult;
  records: Map<string, CaptureAliasRecord>;
  surface: string;
  /** The stub's surface read by the compiler against the repo's packages. */
  member: (alias: string, name: string) => { type: ts.Type; checker: ts.TypeChecker };
}

function captureService(scratch: string, name: string, routes: string, extra: CaptureAnchorRequest[] = []): Capture {
  const repoRoot = path.join(scratch, name);
  writeService(repoRoot, routes);
  const at = (rel: string) => path.join(repoRoot, rel);
  const literal = (alias: string, type_text: string, printed_names?: PrintedName[]): CaptureAnchorRequest => ({
    kind: 'literal', alias, type_text, anchor_origin: 'deterministic-infer', source_file: 'src/router.ts',
    ...(printed_names ? { printed_names } : {}),
  });
  const entry = at('node_modules/ledgerkit/index.d.ts');

  const result = captureStub({
    repoRoot,
    serviceName: name,
    outDir: path.join(scratch, `${name}-stub`),
    anchors: [
      // Declared in the package's entry.
      literal('Kept_Entry', '{ account: Account; }', [
        { name: 'Account', file: entry, export_path: ['Account'] },
      ]),
      // Declared in a file an `exports` subpath reaches.
      literal('Kept_Subpath', '{ held: Money | null; history: Money[]; }', [
        { name: 'Money', file: at('node_modules/ledgerkit/money.d.ts'), export_path: ['Money'] },
      ]),
      // A namespace member printed bare is imported through its namespace.
      literal('Kept_Namespace', '{ entry: Entry; }', [
        { name: 'Entry', file: entry, export_path: ['Ledger', 'Entry'] },
      ]),
      // A package only another package depends on, installed where the entry
      // resolves it.
      literal('Kept_Transitive', '{ balance: Decimal; }', [
        { name: 'Decimal', file: at('node_modules/decimalkit/index.d.ts'), export_path: ['Decimal'] },
      ]),
      // Two packages export `Token`; the text was printed for one of them.
      literal('Kept_OneOfTwo', '{ token: Token; }', [
        { name: 'Token', file: at('node_modules/beta/index.d.ts'), export_path: ['Token'] },
      ]),
      // A repo module's name keeps the #1836 import.
      literal('Kept_Repo', '{ status: Status; }', [
        { name: 'Status', file: at('src/model.ts'), export_path: ['Status'] },
      ]),
      // Printed for both packages' `Token`: the text no longer says which.
      literal('Bare_BothOfTwo', '{ first: Token; second: Token; }', [
        { name: 'Token', file: at('node_modules/alpha/index.d.ts'), export_path: ['Token'] },
        { name: 'Token', file: at('node_modules/beta/index.d.ts'), export_path: ['Token'] },
      ]),
      // A file `exports` hides, which the entry does not re-export from.
      literal('Bare_NotExported', '{ audit: AuditTrail; }', [
        { name: 'AuditTrail', file: at('node_modules/ledgerkit/internal/audit.d.ts'), export_path: ['AuditTrail'] },
      ]),
      // A file `exports` hides, although the entry re-exports the name: no
      // specifier resolves to the declaring file.
      literal('Bare_ReExported', '{ rate: Rate; }', [
        { name: 'Rate', file: at('node_modules/ledgerkit/internal/rate.d.ts'), export_path: ['Rate'] },
      ]),
      // A package the entry cannot resolve by name.
      literal('Bare_Nested', '{ hidden: Hidden; }', [
        { name: 'Hidden', file: at('node_modules/ledgerkit/node_modules/hiddenkit/index.d.ts'), export_path: ['Hidden'] },
      ]),
      // A workspace package linked in from outside the service root.
      literal('Bare_Workspace', '{ shared: Shared; }', [
        { name: 'Shared', file: `${repoRoot}-shared/index.ts`, export_path: ['Shared'] },
      ]),
      // A dependency that is not installed: no declaration to import.
      literal('Bare_NotInstalled', '{ ghost: Ghost; }', [
        { name: 'Ghost', file: at('node_modules/ghostkit/index.d.ts'), export_path: ['Ghost'] },
      ]),
      // A record with no export path names nothing.
      literal('Bare_NoExportPath', '{ account: Account; }', [
        { name: 'Account', file: entry, export_path: [] },
      ]),
      ...extra,
    ],
  });
  assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);

  // Read the stub as a consumer of the stub would: its surface, with the
  // repo's installed packages beside it.
  const link = path.join(result.stub_dir, 'node_modules');
  fs.symlinkSync(path.join(repoRoot, 'node_modules'), link, 'dir');
  const surfacePath = path.join(result.stub_dir, 'types', 'surface.d.ts');
  const program = ts.createProgram([surfacePath], {
    strict: true, noEmit: true, target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler, types: [],
  });
  const source = program.getSourceFile(surfacePath)!;
  const checker = program.getTypeChecker();
  const member = (alias: string, memberName: string) => {
    const declaration = source.statements.find(
      (s): s is ts.TypeAliasDeclaration => ts.isTypeAliasDeclaration(s) && s.name.text === alias
    );
    assert.ok(declaration, `surface must declare ${alias}`);
    const property = checker.getPropertyOfType(checker.getTypeAtLocation(declaration), memberName);
    assert.ok(property, `no member ${memberName} on ${alias}`);
    return { type: checker.getTypeOfSymbol(property), checker };
  };
  return {
    repoRoot,
    result,
    records: new Map(result.aliases.map((record) => [record.alias, record])),
    surface: fs.readFileSync(surfacePath, 'utf8'),
    member,
  };
}

const WHOLE_ROUTES = 'export interface RouteReply { id: string }\n';

/** Members of a type, by name, as the compiler reads them in the stub. */
function memberNames(read: { type: ts.Type; checker: ts.TypeChecker }): string[] {
  const type = read.checker.getNonNullableType(read.type);
  assert.ok(
    !(type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)),
    `reads ${read.checker.typeToString(type)}`
  );
  return read.checker.getPropertiesOfType(type).map((property) => property.getName()).sort();
}

const readsAny = (read: { type: ts.Type }): boolean => (read.type.flags & ts.TypeFlags.Any) !== 0;

const KEPT = ['Kept_Entry', 'Kept_Subpath', 'Kept_Namespace', 'Kept_Transitive', 'Kept_OneOfTwo', 'Kept_Repo'];

describe('capture imports a printed name from the installed package that declares it (#1855)', () => {
  let scratch: string;
  let capture: Capture;

  before(() => {
    // Real path: the compiler resolves a package to its real path.
    scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1855-')));
    capture = captureService(scratch, 'whole-svc', WHOLE_ROUTES);
  });

  after(() => {
    fs.rmSync(scratch, { recursive: true, force: true });
  });

  it('a type the package entry declares is imported by the package name and pinned', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Kept_Entry'),
      'export type Kept_Entry = { account: import("ledgerkit").Account; };'
    );
    assert.deepStrictEqual(
      memberNames(capture.member('Kept_Entry', 'account')),
      ['audit', 'balance', 'held', 'hidden', 'id']
    );
    assert.strictEqual(capture.result.pinned_dependencies.ledgerkit, '2.1.0');
  });

  it('a type in a file an exports subpath reaches is imported by that subpath', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Kept_Subpath'),
      'export type Kept_Subpath = { held: import("ledgerkit/money").Money | null; history: import("ledgerkit/money").Money[]; };'
    );
    assert.deepStrictEqual(memberNames(capture.member('Kept_Subpath', 'held')), ['cents', 'currency']);
    const history = capture.member('Kept_Subpath', 'history');
    assert.ok(history.checker.isArrayType(history.type), history.checker.typeToString(history.type));
  });

  it('a namespace member printed bare is imported through its namespace', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Kept_Namespace'),
      'export type Kept_Namespace = { entry: import("ledgerkit").Ledger.Entry; };'
    );
    assert.deepStrictEqual(memberNames(capture.member('Kept_Namespace', 'entry')), ['posted', 'ref']);
  });

  it('a package only another package depends on is imported and pinned at its installed version', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Kept_Transitive'),
      'export type Kept_Transitive = { balance: import("decimalkit").Decimal; };'
    );
    assert.deepStrictEqual(
      memberNames(capture.member('Kept_Transitive', 'balance')),
      ['digits', 'plus', 'toJSON']
    );
    // The repo's own manifest does not name it: the pin is the installed copy's.
    assert.strictEqual(capture.result.pinned_dependencies.decimalkit, '10.4.3');
  });

  it('a name two packages export is imported from the one it was printed for', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Kept_OneOfTwo'),
      'export type Kept_OneOfTwo = { token: import("beta").Token; };'
    );
    assert.deepStrictEqual(memberNames(capture.member('Kept_OneOfTwo', 'token')), ['b']);
    assert.strictEqual(capture.result.pinned_dependencies.beta, '1.0.0');
  });

  it("a repo module's name keeps its import from that module", () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Kept_Repo'),
      'export type Kept_Repo = { status: import("./src/model").Status; };'
    );
  });

  it('every imported alias self-checks ok with nothing left unresolved', () => {
    for (const alias of KEPT) {
      const record = capture.records.get(alias)!;
      assert.strictEqual(record.self_check, 'ok', `${alias}: ${record.self_check_detail}`);
      assert.strictEqual(record.unresolved_in_tree, undefined, alias);
      assert.strictEqual(record.capture_failure_reason, undefined, alias);
    }
  });

  it('a name printed for both packages is left as written', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Bare_BothOfTwo'),
      'export type Bare_BothOfTwo = { first: Token; second: Token; };'
    );
    assert.ok(readsAny(capture.member('Bare_BothOfTwo', 'first')));
    // Neither package is pinned on this alias's account.
    assert.strictEqual(capture.result.pinned_dependencies.alpha, undefined);
  });

  it('a type in a file the exports hide is left as written, re-exported by the entry or not', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Bare_NotExported'),
      'export type Bare_NotExported = { audit: AuditTrail; };'
    );
    assert.strictEqual(
      surfaceLine(capture.surface, 'Bare_ReExported'),
      'export type Bare_ReExported = { rate: Rate; };'
    );
    assert.ok(readsAny(capture.member('Bare_NotExported', 'audit')));
    assert.ok(readsAny(capture.member('Bare_ReExported', 'rate')));
  });

  it('a package the entry cannot resolve by name is left as written', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Bare_Nested'),
      'export type Bare_Nested = { hidden: Hidden; };'
    );
    assert.strictEqual(capture.result.pinned_dependencies.hiddenkit, undefined);
  });

  it('a workspace package linked in from outside the service root is left as written', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Bare_Workspace'),
      'export type Bare_Workspace = { shared: Shared; };'
    );
    assert.strictEqual(capture.result.pinned_dependencies['@ws/shared'], undefined);
  });

  it('a package that is not installed is not imported, and the record says the member did not resolve', () => {
    // Nothing in the program declares the name, so the member reads `unknown`
    // with its cause (#1377), as it did before this change.
    assert.strictEqual(
      surfaceLine(capture.surface, 'Bare_NotInstalled'),
      'export type Bare_NotInstalled = { ghost: unknown; };'
    );
    assert.strictEqual(capture.result.pinned_dependencies.ghostkit, undefined);
    const record = capture.records.get('Bare_NotInstalled')!;
    assert.notStrictEqual(record.self_check, 'ok');
    assert.deepStrictEqual(
      (record.any_provenance ?? []).map((finding) => [finding.path, finding.reason]),
      [['ghost', 'unresolved_import']]
    );
  });

  it('a record with no export path names nothing', () => {
    assert.strictEqual(
      surfaceLine(capture.surface, 'Bare_NoExportPath'),
      'export type Bare_NoExportPath = { account: Account; };'
    );
  });

  it('every alias left as written says so on its record', () => {
    for (const alias of ['Bare_BothOfTwo', 'Bare_NotExported', 'Bare_ReExported', 'Bare_Nested', 'Bare_Workspace']) {
      const record = capture.records.get(alias)!;
      assert.ok((record.unresolved_in_tree ?? []).length > 0, `${alias}: ${JSON.stringify(record)}`);
    }
  });

  it('no declaration in the tree holds a checkout path', () => {
    const typesDir = path.join(capture.result.stub_dir, 'types');
    for (const file of stubFiles(typesDir)) {
      const text = fs.readFileSync(file, 'utf8');
      assert.ok(!text.includes(scratch), `${path.relative(typesDir, file)} holds ${scratch}:\n${text}`);
      assert.ok(!text.includes('node_modules'), `${path.relative(typesDir, file)} names node_modules:\n${text}`);
    }
  });
});

describe('a package import survives a partial emit (#1855, #1773)', () => {
  /** An ambient module whose `export =` hides the type its default export
   * needs: the importing file's declaration is skipped (TS4023). */
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
    'export interface RouteReply { id: string }',
    'export default server;',
    '',
  ].join('\n');

  let scratch: string;
  let partial: Capture;
  let whole: Capture;

  before(() => {
    scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1855-partial-')));
    const skipped: CaptureAnchorRequest = {
      kind: 'symbol', alias: 'Demoted_Reply', symbol_name: 'RouteReply',
      source_file: 'src/http/routes.ts', anchor_origin: 'llm-symbol',
    };
    write(path.join(scratch, 'partial-svc'), 'src/stubs.d.ts', UNNAMEABLE_STUB);
    partial = captureService(scratch, 'partial-svc', SKIPPED_ROUTES, [skipped]);
    whole = captureService(scratch, 'whole-svc', WHOLE_ROUTES, [skipped]);
  });

  after(() => {
    fs.rmSync(scratch, { recursive: true, force: true });
  });

  it('the fixture is a partial emit', () => {
    assert.ok(
      partial.result.errors.some((e) => /declaration emit was partial/.test(e)),
      JSON.stringify(partial.result.errors)
    );
    assert.match(
      partial.records.get('Demoted_Reply')!.capture_failure_reason ?? '',
      /declaration emit was skipped for module/
    );
  });

  it('every imported alias reads exactly as on a whole emit', () => {
    for (const alias of KEPT) {
      assert.strictEqual(surfaceLine(partial.surface, alias), surfaceLine(whole.surface, alias), alias);
      assert.deepStrictEqual(partial.records.get(alias), whole.records.get(alias), alias);
      assert.strictEqual(partial.records.get(alias)!.self_check, 'ok', alias);
    }
    assert.match(surfaceLine(partial.surface, 'Kept_Transitive'), /import\("decimalkit"\)\.Decimal/);
    assert.strictEqual(partial.result.pinned_dependencies.decimalkit, '10.4.3');
  });

  it('no declaration in the tree holds a checkout path', () => {
    const typesDir = path.join(partial.result.stub_dir, 'types');
    for (const file of stubFiles(typesDir)) {
      const text = fs.readFileSync(file, 'utf8');
      assert.ok(!text.includes(scratch), `${path.relative(typesDir, file)} holds ${scratch}:\n${text}`);
      assert.ok(!text.includes('node_modules'), `${path.relative(typesDir, file)} names node_modules:\n${text}`);
    }
  });
});
