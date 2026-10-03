/**
 * carrick#1836: a literal anchor's bare name that its source file never names
 * resolves through the declaration the inference printed it for.
 *
 * The v1 inferrer prints some types with no enclosing declaration, so a
 * member typed by a database client's enum comes back as
 * `status: EntityStatus` although the route's file never imports the enum.
 * #1774 reads names in the anchor's source file, which cannot help: there the
 * name means nothing, the surface member reads `any`, and the record lists it
 * as unresolved (#1446).
 *
 * The inference now records what each such name meant (`printed_names`), and
 * the capture imports the name from the module that declares it, when that
 * module is inside the repo. A name recorded for two declarations is left as
 * written: the text no longer says which one a position meant.
 *
 * The capture's own self-check is not the oracle here; the test reads the
 * stub's surface with the compiler.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub } from '../src/capture/index.js';

const MODEL_TS = `export type Status = 'open' | 'closed';
export enum Color {
  Red = 'RED',
  Blue = 'BLUE',
}
export namespace Billing {
  export type Kind = 'card' | 'transfer';
}
`;

const LEGACY_TS = `export type Status = 'a' | 'b';
`;

/**
 * The route's own file: it reaches the modules its responses' types come
 * from, but binds none of their names.
 */
const ROUTER_TS = `import type * as Repo from './repo';
export function handler(_previous?: Repo.LegacyStatus): void {}
`;

describe('capture imports a printed name from the declaration the inference names (#1836)', () => {
  let parentDir: string;
  let repoDir: string;
  let outDir: string;
  let surfaceText: string;
  let stub: {
    checker: ts.TypeChecker;
    alias: (name: string) => ts.Type;
    diagnostics: Array<{ code: number; text: string }>;
  };

  before(() => {
    // Real path: the compiler resolves a package to its real path, and a
    // symlinked temp dir would put it outside the repo for the wrong reason.
    parentDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1836-capture-')));
    repoDir = path.join(parentDir, 'repo');
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler' },
        include: ['src'],
      })
    );
    const kit = path.join(repoDir, 'node_modules', 'kit');
    fs.mkdirSync(kit, { recursive: true });
    fs.writeFileSync(path.join(kit, 'package.json'), JSON.stringify({ name: 'kit', version: '1.0.0', types: 'index.d.ts' }));
    fs.writeFileSync(path.join(kit, 'index.d.ts'), 'export type Ext = { x: number };\n');
    fs.writeFileSync(path.join(repoDir, 'src', 'model.ts'), MODEL_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'legacy.ts'), LEGACY_TS);
    // The model and the package reach the program the way a route's types
    // reach it: through a module the route's file imports.
    fs.writeFileSync(
      path.join(repoDir, 'src', 'repo.ts'),
      "export type { Status, Color, Billing } from './model';\n" +
        "export type { Status as LegacyStatus } from './legacy';\n" +
        "export type { Ext } from 'kit';\n"
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'router.ts'), ROUTER_TS);
    outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1836-out-'));

    const model = path.join(repoDir, 'src', 'model.ts');
    const legacy = path.join(repoDir, 'src', 'legacy.ts');
    const literal = (
      alias: string,
      type_text: string,
      printed_names?: Array<{ name: string; file: string; export_path: string[] }>
    ) => ({
      kind: 'literal' as const,
      alias,
      type_text,
      anchor_origin: 'deterministic-infer' as const,
      source_file: 'src/router.ts',
      ...(printed_names ? { printed_names } : {}),
    });
    const result = captureStub({
      repoRoot: repoDir,
      serviceName: 'printed-names',
      outDir,
      anchors: [
        literal('Endpoint_row_Response', '{ status: Status; color: Color; tags: Color[]; maybe: Status | null; }', [
          { name: 'Status', file: model, export_path: ['Status'] },
          { name: 'Color', file: model, export_path: ['Color'] },
        ]),
        // A qualified enum member keeps its qualifier after the import.
        literal('Endpoint_member_Response', '{ color: Color.Red; }', [
          { name: 'Color', file: model, export_path: ['Color'] },
        ]),
        // A namespace member printed bare is imported through its namespace.
        literal('Endpoint_kind_Response', '{ kind: Kind; }', [
          { name: 'Kind', file: model, export_path: ['Billing', 'Kind'] },
        ]),
        // Two declarations for one name: the text no longer says which.
        literal('Endpoint_ambiguous_Response', '{ status: Status; }', [
          { name: 'Status', file: model, export_path: ['Status'] },
          { name: 'Status', file: legacy, export_path: ['Status'] },
        ]),
        // A package's type: the stub ships no node_modules (#1789's class).
        literal('Endpoint_package_Response', '{ ext: Ext; }', [
          { name: 'Ext', file: path.join(repoDir, 'node_modules', 'kit', 'index.d.ts'), export_path: ['Ext'] },
        ]),
        // An export path the module does not have names nothing.
        literal('Endpoint_stale_Response', '{ status: Status; }', [
          { name: 'Status', file: model, export_path: ['Statuses'] },
        ]),
        // Without a record the name is left as the text wrote it.
        literal('Endpoint_unrecorded_Response', '{ status: Status; }'),
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);

    const surface = path.join(result.stub_dir, 'types', 'surface.d.ts');
    surfaceText = fs.readFileSync(surface, 'utf-8').replace(/\s+/g, ' ');
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
      diagnostics: ts.getPreEmitDiagnostics(program, source).map((d) => ({
        code: d.code,
        text: ts.flattenDiagnosticMessageText(d.messageText, ' '),
      })),
    };
  });

  after(() => {
    fs.rmSync(parentDir, { recursive: true, force: true });
    fs.rmSync(outDir, { recursive: true, force: true });
  });

  function memberType(type: ts.Type, member: string): ts.Type {
    const property = stub.checker.getPropertyOfType(type, member);
    assert.ok(property, `no member ${member} on ${stub.checker.typeToString(type)}`);
    return stub.checker.getTypeOfSymbol(property);
  }

  /** The literal values of a member's type, with null and undefined dropped. */
  function literals(type: ts.Type, member: string): Array<string | number> {
    const resolved = stub.checker.getNonNullableType(memberType(type, member));
    assert.ok(
      !(resolved.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)),
      `${member} reads ${stub.checker.typeToString(resolved)}\n${surfaceText}`
    );
    const parts = resolved.isUnion() ? resolved.types : [resolved];
    return parts
      .map((part) => {
        assert.ok(part.isLiteral(), `${member} has a non-literal part ${stub.checker.typeToString(part)}`);
        return part.value as string | number;
      })
      .sort();
  }

  const readsAny = (alias: string, member: string): boolean =>
    (memberType(stub.alias(alias), member).flags & ts.TypeFlags.Any) !== 0;

  it('a recorded name resolves to the declaration it was printed for', () => {
    const row = stub.alias('Endpoint_row_Response');
    assert.deepStrictEqual(literals(row, 'status'), ['closed', 'open']);
    assert.deepStrictEqual(literals(row, 'color'), ['BLUE', 'RED']);
    assert.deepStrictEqual(literals(row, 'maybe'), ['closed', 'open']);
    const tags = memberType(row, 'tags');
    assert.ok(stub.checker.isArrayType(tags), stub.checker.typeToString(tags));
  });

  it('a qualified enum member keeps its qualifier', () => {
    assert.deepStrictEqual(literals(stub.alias('Endpoint_member_Response'), 'color'), ['RED']);
  });

  it('a namespace member printed bare is imported through its namespace', () => {
    assert.deepStrictEqual(literals(stub.alias('Endpoint_kind_Response'), 'kind'), ['card', 'transfer']);
  });

  it('a name recorded for two declarations is left as written', () => {
    assert.ok(readsAny('Endpoint_ambiguous_Response', 'status'), surfaceText);
    assert.match(surfaceText, /export type Endpoint_ambiguous_Response = \{ status: Status; \};/);
  });

  it('a package type is not imported by path', () => {
    assert.match(surfaceText, /export type Endpoint_package_Response = \{ ext: Ext; \};/);
    assert.ok(!surfaceText.includes('node_modules'), surfaceText);
  });

  it('a record whose export path the module lacks changes nothing', () => {
    assert.match(surfaceText, /export type Endpoint_stale_Response = \{ status: Status; \};/);
  });

  it('an unrecorded name is left as written', () => {
    assert.ok(readsAny('Endpoint_unrecorded_Response', 'status'), surfaceText);
  });

  it('the only names the surface cannot resolve are the ones left as written', () => {
    const unresolved = stub.diagnostics
      .filter((d) => d.code === 2304 || d.code === 2503 || d.code === 2552)
      .map((d) => /Cannot find (?:name|namespace) '([^']+)'/.exec(d.text)?.[1] ?? d.text)
      .sort();
    assert.deepStrictEqual(unresolved, ['Ext', 'Status', 'Status', 'Status']);
  });
});
