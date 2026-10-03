/**
 * carrick#1774: a literal anchor's text names types as they read where it was
 * printed, and the surface entry reads it somewhere else.
 *
 * The v1 inferrer prints a type at its node, where a module's own aliases are
 * in scope, so a field typed by a string-union alias comes back as
 * `status?: InvoiceStatus | null`. The capture wrote that text into the
 * surface entry, where `InvoiceStatus` names nothing: the stub's member read
 * `any`, and the pair fell to unverifiable. The capture's own self-check said
 * `ok`, so this test reads the stub with the compiler instead.
 *
 * A name the text prints is now read in the anchor's source file. When it
 * means a type a repo module exports, the surface imports it from there; when
 * the source imports it from a package, the surface imports it from that
 * package (#1789).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { spawnSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub } from '../src/capture/index.js';

const GENERATED_TS = `import type { Currency, Currency as Money } from './money';
export type InvoiceStatus =
  | 'DRAFT'
  | 'SENT';
export enum Channel {
  Email = 'EMAIL',
  Post = 'POST',
}
/** A domain type sharing its name with a DOM global. */
export interface Notification {
  id: string;
  read: boolean;
}
export type Maybe<T> = T | null;
export type Invoice = { status: InvoiceStatus; total: Currency; cost: Money };
`;

const MONEY_TS = `export type Currency = 'EUR' | 'USD';
`;

describe('capture reads a literal anchor\'s names where the text was printed (#1774)', () => {
  let repoDir: string;
  let outDir: string;
  let stub: { checker: ts.TypeChecker; alias: (name: string) => ts.Type; diagnostics: string[] };

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1774-capture-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler' },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'generated.ts'), GENERATED_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'money.ts'), MONEY_TS);
    outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1774-out-'));

    const literal = (alias: string, type_text: string) => ({
      kind: 'literal' as const,
      alias,
      type_text,
      anchor_origin: 'deterministic-infer' as const,
      source_file: 'src/generated.ts',
    });
    const result = captureStub({
      repoRoot: repoDir,
      serviceName: 'literal-names',
      outDir,
      anchors: [
        literal(
          'Endpoint_status_Response',
          '{ __typename: "Invoice"; id: string; status?: InvoiceStatus | null; } | null'
        ),
        // Imported into the source file from another module.
        literal('Endpoint_total_Response', '{ total: Currency; }'),
        // Imported under another name: the module's own export name is used.
        literal('Endpoint_cost_Response', '{ cost: Money; }'),
        // An enum member, through a qualified name.
        literal('Endpoint_channel_Response', '{ channel: Channel.Email; }'),
        // The destination's DOM global of the same name is not what was printed.
        literal('Endpoint_notice_Response', '{ notice: Notification; }'),
        // Type arguments ride along, and are read in the same scope.
        literal('Endpoint_maybe_Response', '{ maybe: Maybe<InvoiceStatus>; }'),
        // A type parameter the text declares is its own, never a module's.
        literal('Endpoint_mapped_Response', '{ [Invoice in InvoiceStatus]: Invoice; }'),
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);

    const surface = path.join(result.stub_dir, 'types', 'surface.d.ts');
    const program = ts.createProgram([surface], {
      strict: true,
      noEmit: true,
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ESNext,
      moduleResolution: ts.ModuleResolutionKind.Bundler,
      types: [],
    });
    const source = program.getSourceFile(surface)!;
    const checker = program.getTypeChecker();
    stub = {
      checker,
      alias: (name: string) => {
        const decl = source.statements.find(
          (s): s is ts.TypeAliasDeclaration => ts.isTypeAliasDeclaration(s) && s.name.text === name
        );
        assert.ok(decl, `surface must declare ${name}:\n${source.text}`);
        return checker.getTypeAtLocation(decl);
      },
      diagnostics: ts
        .getPreEmitDiagnostics(program, source)
        .map((d) => ts.flattenDiagnosticMessageText(d.messageText, ' ')),
    };
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outDir, { recursive: true, force: true });
  });

  /** The literal values of a member's type, with null and undefined dropped. */
  function literals(type: ts.Type, member: string): Array<string | number> {
    const property = stub.checker.getPropertyOfType(stub.checker.getNonNullableType(type), member);
    assert.ok(property, `no member ${member} on ${stub.checker.typeToString(type)}`);
    const memberType = stub.checker.getNonNullableType(stub.checker.getTypeOfSymbol(property));
    assert.ok(
      !(memberType.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)),
      `${member} reads ${stub.checker.typeToString(memberType)}`
    );
    const parts = memberType.isUnion() ? memberType.types : [memberType];
    return parts
      .map((part) => {
        assert.ok(part.isLiteral(), `${member} has a non-literal part ${stub.checker.typeToString(part)}`);
        return part.value as string | number;
      })
      .sort();
  }

  it('the surface names nothing it cannot resolve', () => {
    assert.deepStrictEqual(stub.diagnostics, []);
  });

  it('a string-union alias the source module declares publishes the union', () => {
    assert.deepStrictEqual(literals(stub.alias('Endpoint_status_Response'), 'status'), ['DRAFT', 'SENT']);
  });

  it('a name imported into the source module resolves to its declaration', () => {
    assert.deepStrictEqual(literals(stub.alias('Endpoint_total_Response'), 'total'), ['EUR', 'USD']);
  });

  it('a name imported under another name imports the export', () => {
    assert.deepStrictEqual(literals(stub.alias('Endpoint_cost_Response'), 'cost'), ['EUR', 'USD']);
  });

  it('a qualified enum member keeps its qualifier', () => {
    assert.deepStrictEqual(literals(stub.alias('Endpoint_channel_Response'), 'channel'), ['EMAIL']);
  });

  it('a name the destination also declares means what it meant in the source', () => {
    const notice = stub.alias('Endpoint_notice_Response');
    const property = stub.checker.getPropertyOfType(notice, 'notice')!;
    const noticeType = stub.checker.getTypeOfSymbol(property);
    assert.deepStrictEqual(
      stub.checker.getPropertiesOfType(noticeType).map((p) => p.name).sort(),
      ['id', 'read']
    );
  });

  it('type arguments are read in the source scope too', () => {
    const maybe = stub.alias('Endpoint_maybe_Response');
    assert.deepStrictEqual(literals(maybe, 'maybe'), ['DRAFT', 'SENT']);
  });

  it('a type parameter the text declares is not rewritten', () => {
    const mapped = stub.alias('Endpoint_mapped_Response');
    assert.deepStrictEqual(literals(mapped, 'DRAFT'), ['DRAFT']);
    assert.deepStrictEqual(literals(mapped, 'SENT'), ['SENT']);
  });
});

/**
 * carrick#1789: a name the source file imports from a package is imported in
 * the surface through the specifier the source wrote, so the check phase's
 * pinned install resolves it. Before, it stayed bare: the stub ships no
 * `node_modules`, the name named nothing there, and the member read `any`.
 */
describe('a name the source imports from a package resolves through that package (#1789)', () => {
  let parentDir: string;
  let outDir: string;
  let captured: ReturnType<typeof captureStub>;
  let surface: string;
  let stub: { checker: ts.TypeChecker; member: (alias: string, member: string) => ts.Type };

  before(() => {
    // Real path: the compiler resolves a package to its real path, and a
    // symlinked temp dir would put it outside the repo for the wrong reason.
    parentDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1789-')));
    const repoDir = path.join(parentDir, 'repo');
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          target: 'es2022',
          module: 'esnext',
          moduleResolution: 'bundler',
          // A bare-looking specifier that names a module outside the repo,
          // not a package.
          paths: { '@shared/*': ['../shared/*'] },
        },
        include: ['src'],
      })
    );
    const pkg = path.join(repoDir, 'node_modules', '@example', 'money');
    fs.mkdirSync(pkg, { recursive: true });
    fs.writeFileSync(
      path.join(pkg, 'package.json'),
      JSON.stringify({
        name: '@example/money',
        version: '1.0.0',
        exports: { '.': { types: './index.d.ts' }, './rates': { types: './rates.d.ts' } },
      })
    );
    fs.writeFileSync(
      path.join(pkg, 'index.d.ts'),
      [
        'export type LibMoney = { cents: number };',
        'export default interface Ledger { entries: number }',
        '',
      ].join('\n')
    );
    fs.writeFileSync(path.join(pkg, 'rates.d.ts'), 'export type Rate = { bps: number };\n');
    // Another version of the same package, installed beside one source only:
    // the entry would name the root install's type, which is not this one.
    const nested = path.join(repoDir, 'src', 'legacy', 'node_modules', '@example', 'money');
    fs.mkdirSync(nested, { recursive: true });
    fs.writeFileSync(
      path.join(nested, 'package.json'),
      JSON.stringify({ name: '@example/money', version: '0.9.0', types: './index.d.ts' })
    );
    fs.writeFileSync(path.join(nested, 'index.d.ts'), 'export type LibMoney = { pence: number };\n');
    fs.writeFileSync(
      path.join(repoDir, 'src', 'legacy', 'page.ts'),
      "import type { LibMoney } from '@example/money';\nexport type Old = LibMoney;\n"
    );
    fs.mkdirSync(path.join(parentDir, 'shared'), { recursive: true });
    fs.writeFileSync(path.join(parentDir, 'shared', 'types.ts'), 'export type Shared = { s: string };\n');
    fs.writeFileSync(path.join(parentDir, 'shared', 'aliased.ts'), 'export type Aliased = { a: string };\n');
    fs.writeFileSync(
      path.join(repoDir, 'src', 'page.ts'),
      [
        "import type { LibMoney, LibMoney as Cash } from '@example/money';",
        "import type Ledger from '@example/money';",
        "import type * as Money from '@example/money';",
        "import type { Rate } from '@example/money/rates';",
        "import type { Shared } from '../../shared/types';",
        "import type { Aliased } from '@shared/aliased';",
        'export type Local = LibMoney | Cash | Ledger | Money.LibMoney | Rate | Shared | Aliased;',
        '',
      ].join('\n')
    );
    outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1789-out-'));
    const literal = (alias: string, type_text: string) => ({
      kind: 'literal' as const,
      alias,
      type_text,
      anchor_origin: 'deterministic-infer' as const,
      source_file: 'src/page.ts',
    });
    captured = captureStub({
      repoRoot: repoDir,
      serviceName: 'literal-names-package',
      outDir,
      anchors: [
        literal('Endpoint_library_Response', '{ price: LibMoney; }'),
        // Imported under another name: the package's own export name is used.
        literal('Endpoint_renamed_Response', '{ cash: Cash; }'),
        // A default import names the package's default export.
        literal('Endpoint_default_Response', '{ ledger: Ledger; }'),
        // A namespace import's qualifier names the export.
        literal('Endpoint_namespace_Response', '{ total: Money.LibMoney; }'),
        // A subpath is kept as the source wrote it.
        literal('Endpoint_subpath_Response', '{ rate: Rate; }'),
        // Type arguments are read in the source scope too.
        literal('Endpoint_list_Response', '{ prices: Array<LibMoney>; }'),
        literal('Endpoint_outside_Response', '{ shared: Shared; }'),
        literal('Endpoint_aliased_Response', '{ aliased: Aliased; }'),
        {
          ...literal('Endpoint_nested_Response', '{ old: LibMoney; }'),
          source_file: 'src/legacy/page.ts',
        },
      ],
    });
    assert.ok(captured.success, `capture failed: ${JSON.stringify(captured.errors)}`);
    surface = fs
      .readFileSync(path.join(captured.stub_dir, 'types', 'surface.d.ts'), 'utf-8')
      .replace(/\s+/g, ' ');

    // What the check phase installs: the stub's pinned dependencies beside it.
    const installed = path.join(captured.stub_dir, 'node_modules', '@example', 'money');
    fs.mkdirSync(path.dirname(installed), { recursive: true });
    fs.cpSync(pkg, installed, { recursive: true });
    const surfaceFile = path.join(captured.stub_dir, 'types', 'surface.d.ts');
    const program = ts.createProgram([surfaceFile], {
      strict: true,
      noEmit: true,
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ESNext,
      moduleResolution: ts.ModuleResolutionKind.Bundler,
      types: [],
    });
    const source = program.getSourceFile(surfaceFile)!;
    const checker = program.getTypeChecker();
    stub = {
      checker,
      member: (alias: string, member: string) => {
        const decl = source.statements.find(
          (s): s is ts.TypeAliasDeclaration => ts.isTypeAliasDeclaration(s) && s.name.text === alias
        );
        assert.ok(decl, `surface must declare ${alias}:\n${source.text}`);
        const property = checker.getPropertyOfType(checker.getTypeAtLocation(decl), member);
        assert.ok(property, `no member ${member} on ${alias}`);
        return checker.getTypeOfSymbol(property);
      },
    };
  });

  after(() => {
    fs.rmSync(parentDir, { recursive: true, force: true });
    fs.rmSync(outDir, { recursive: true, force: true });
  });

  /** The member names of a member's type; fails on `any`/`unknown`. */
  function shape(type: ts.Type): string[] {
    assert.ok(
      !(type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)),
      `reads ${stub.checker.typeToString(type)}`
    );
    return stub.checker.getPropertiesOfType(type).map((p) => p.name).sort();
  }

  it('imports the name through the specifier the source wrote', () => {
    assert.match(surface, /Endpoint_library_Response = \{ price: import\("@example\/money"\)\.LibMoney; \};/);
    assert.match(surface, /Endpoint_renamed_Response = \{ cash: import\("@example\/money"\)\.LibMoney; \};/);
    assert.match(surface, /Endpoint_default_Response = \{ ledger: import\("@example\/money"\)\.default; \};/);
    assert.match(surface, /Endpoint_namespace_Response = \{ total: import\("@example\/money"\)\.LibMoney; \};/);
    assert.match(surface, /Endpoint_subpath_Response = \{ rate: import\("@example\/money\/rates"\)\.Rate; \};/);
    assert.ok(!surface.includes('node_modules'), surface);
  });

  it('pins the package the surface now imports', () => {
    assert.deepStrictEqual(captured.pinned_dependencies, { '@example/money': '1.0.0' });
    assert.deepStrictEqual(captured.unpinned_externals, []);
  });

  it('the member resolves once the pinned package is installed', () => {
    assert.deepStrictEqual(shape(stub.member('Endpoint_library_Response', 'price')), ['cents']);
    assert.deepStrictEqual(shape(stub.member('Endpoint_renamed_Response', 'cash')), ['cents']);
    assert.deepStrictEqual(shape(stub.member('Endpoint_default_Response', 'ledger')), ['entries']);
    assert.deepStrictEqual(shape(stub.member('Endpoint_namespace_Response', 'total')), ['cents']);
    assert.deepStrictEqual(shape(stub.member('Endpoint_subpath_Response', 'rate')), ['bps']);
    const prices = stub.member('Endpoint_list_Response', 'prices');
    const element = stub.checker.getIndexTypeOfType(prices, ts.IndexKind.Number);
    assert.ok(element, `prices reads ${stub.checker.typeToString(prices)}`);
    assert.deepStrictEqual(shape(element), ['cents']);
  });

  it('a type declared outside the repo is not imported by path', () => {
    assert.match(surface, /Endpoint_outside_Response = \{ shared: Shared; \};/);
    assert.ok(!surface.includes('shared/types'), surface);
  });

  it('a path alias to a module outside the repo is not mistaken for a package', () => {
    assert.match(surface, /Endpoint_aliased_Response = \{ aliased: Aliased; \};/);
    assert.ok(!surface.includes('@shared/aliased'), surface);
  });

  it('a specifier the entry resolves to another copy of the package is not used', () => {
    assert.match(surface, /Endpoint_nested_Response = \{ old: LibMoney; \};/);
  });
});

const hasDeno = spawnSync('deno', ['--version']).status === 0;

/**
 * carrick#1789 on Deno: the source names a package through an import-map key
 * or an `npm:` specifier, neither of which the stub's npm install reads. The
 * surface names the npm package, and the capture pins its cached version.
 */
describe('a name a Deno source imports from an npm package resolves through that package (#1789)', { skip: !hasDeno }, () => {
  it('imports the name by its npm package name and pins it', () => {
    const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1789-deno-')));
    try {
      fs.writeFileSync(
        path.join(root, 'deno.json'),
        JSON.stringify({ imports: { hono: 'npm:hono@4.12.12', web: 'npm:hono@4.12.12' } })
      );
      fs.writeFileSync(
        path.join(root, 'main.ts'),
        [
          'import type { Env } from "hono";',
          'import type { Env as Direct } from "npm:hono@4.12.12";',
          // An import-map key that is not the package's name: the entry
          // cannot import by it, so the name stays as the text printed it.
          'import type { Schema } from "web";',
          'export type Local = Env | Direct | Schema;',
          '',
        ].join('\n')
      );
      const installed = spawnSync('deno', ['install', '--node-modules-dir=none'], { cwd: root, encoding: 'utf8' });
      assert.equal(installed.status, 0, installed.stderr);
      const result = captureStub({
        repoRoot: root,
        serviceName: 'deno-literal-package',
        outDir: path.join(root, '.carrick', 'stub'),
        anchors: [
          {
            kind: 'literal',
            alias: 'Endpoint_mapped_Response',
            type_text: '{ env: Env; }',
            anchor_origin: 'deterministic-infer',
            source_file: 'main.ts',
          },
          {
            kind: 'literal',
            alias: 'Endpoint_direct_Response',
            type_text: '{ env: Direct; }',
            anchor_origin: 'deterministic-infer',
            source_file: 'main.ts',
          },
          {
            kind: 'literal',
            alias: 'Endpoint_keyed_Response',
            type_text: '{ schema: Schema; }',
            anchor_origin: 'deterministic-infer',
            source_file: 'main.ts',
          },
        ],
      });
      assert.ok(result.success, result.errors.join('\n'));
      const text = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8').replace(/\s+/g, ' ');
      assert.match(text, /Endpoint_mapped_Response = \{ env: import\("hono"\)\.Env; \};/);
      assert.match(text, /Endpoint_direct_Response = \{ env: import\("hono"\)\.Env; \};/);
      assert.match(text, /Endpoint_keyed_Response = \{ schema: Schema; \};/);
      assert.ok(!text.includes('import("web")'), text);
      assert.equal(result.pinned_dependencies.hono, '4.12.12');
      for (const alias of ['Endpoint_mapped_Response', 'Endpoint_direct_Response']) {
        const record = result.aliases.find((entry) => entry.alias === alias);
        assert.equal(record?.self_check, 'ok', JSON.stringify(record));
      }
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
