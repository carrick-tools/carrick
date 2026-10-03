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
 * means a type a repo module exports, the surface imports it from there.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
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

describe('a name the stub does not ship stays as written (#1774)', () => {
  let parentDir: string;
  let outDir: string;
  let surface: string;

  before(() => {
    // Real path: the compiler resolves a package to its real path, and a
    // symlinked temp dir would put it outside the repo for the wrong reason.
    parentDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1774-outside-')));
    const repoDir = path.join(parentDir, 'repo');
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler' },
        include: ['src'],
      })
    );
    const pkg = path.join(repoDir, 'node_modules', '@example', 'money');
    fs.mkdirSync(pkg, { recursive: true });
    fs.writeFileSync(
      path.join(pkg, 'package.json'),
      JSON.stringify({ name: '@example/money', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(pkg, 'index.d.ts'), 'export type LibMoney = { cents: number };\n');
    fs.mkdirSync(path.join(parentDir, 'shared'), { recursive: true });
    fs.writeFileSync(path.join(parentDir, 'shared', 'types.ts'), 'export type Shared = { s: string };\n');
    fs.writeFileSync(
      path.join(repoDir, 'src', 'page.ts'),
      [
        "import type { LibMoney } from '@example/money';",
        "import type { Shared } from '../../shared/types';",
        'export type Local = LibMoney | Shared;',
        '',
      ].join('\n')
    );
    outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1774-out-'));
    const result = captureStub({
      repoRoot: repoDir,
      serviceName: 'literal-names-outside',
      outDir,
      anchors: [
        {
          kind: 'literal',
          alias: 'Endpoint_library_Response',
          type_text: '{ price: LibMoney; }',
          anchor_origin: 'deterministic-infer',
          source_file: 'src/page.ts',
        },
        {
          kind: 'literal',
          alias: 'Endpoint_outside_Response',
          type_text: '{ shared: Shared; }',
          anchor_origin: 'deterministic-infer',
          source_file: 'src/page.ts',
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    surface = fs
      .readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf-8')
      .replace(/\s+/g, ' ');
  });

  after(() => {
    fs.rmSync(parentDir, { recursive: true, force: true });
    fs.rmSync(outDir, { recursive: true, force: true });
  });

  it('a type under node_modules is not imported by path', () => {
    assert.match(surface, /export type Endpoint_library_Response = \{ price: LibMoney; \};/);
    assert.ok(!surface.includes('node_modules'), surface);
  });

  it('a type declared outside the repo is not imported by path', () => {
    assert.match(surface, /export type Endpoint_outside_Response = \{ shared: Shared; \};/);
    assert.ok(!surface.includes('shared/types'), surface);
  });
});
