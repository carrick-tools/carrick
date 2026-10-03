/**
 * A declaration emitted for a source outside the emit's rootDir lands inside
 * the stub (carrick#1770).
 *
 * A service that reads a sibling package's SOURCE (a `paths` mapping or a
 * relative import into `../../packages/...`) puts files outside its rootDir in
 * the capture program. tsc does not emit their declarations under `outDir`:
 * it hands the write callback the source's own path with `.d.ts`. The
 * relocation used to join that onto the stub's `types/` dir by its path
 * relative to the staging dir, which climbs out of the stub. Where the climbed
 * path could not be created the capture failed; where it could, the
 * declaration was written outside the stub and the stub referenced a file the
 * check never copies.
 *
 * The stub must stand alone: copied anywhere, its surface type-checks and its
 * aliases read the sibling package's types.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub } from '../src/capture/index.js';

const FILES: Record<string, string> = {
  'packages/core/src/money.ts': 'export interface Money { amount: number; currency: string }\n',
  'packages/core/src/ids.ts': [
    'export interface OrderId { value: string }',
    "export function newId(): OrderId { return { value: 'o_1' }; }",
    '',
  ].join('\n'),
  // Two outside files that import each other by a relative path.
  'packages/core/src/schemas/order.ts': [
    "import type { Money } from '../money';",
    'export interface CoreOrder { id: string; total: Money }',
    '',
  ].join('\n'),
  'apps/web/package.json': JSON.stringify({ name: 'web', private: true }),
  'apps/web/tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'ESNext',
      moduleResolution: 'Bundler',
      strict: true,
      skipLibCheck: true,
      paths: { '@core/*': ['../../packages/core/src/*'] },
    },
    include: ['src'],
  }),
  'apps/web/src/routes.ts': [
    // One sibling module through a `paths` mapping, one by a relative path.
    "import type { CoreOrder } from '@core/schemas/order';",
    "import type { Money } from '../../../packages/core/src/money';",
    "import { newId } from '../../../packages/core/src/ids';",
    '',
    'export interface WebOrder {',
    '  order: CoreOrder;',
    '  refund: Money;',
    '}',
    '',
    // An inferred return type that names an outside module only by inference.
    'export function latest(orders: CoreOrder[]) {',
    '  return { first: orders[0], count: orders.length };',
    '}',
    '',
    // A type this file never names: the declaration writes import("...").
    'export function created() {',
    '  return { id: newId() };',
    '}',
    '',
  ].join('\n'),
};

/** Every file, directory and link under `dir`, with a content hash. */
function snapshot(dir: string): Record<string, string> {
  const out: Record<string, string> = {};
  const walk = (current: string) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const abs = path.join(current, entry.name);
      const rel = path.relative(dir, abs);
      if (entry.isSymbolicLink()) out[rel] = `link:${fs.readlinkSync(abs)}`;
      else if (entry.isDirectory()) {
        out[`${rel}/`] = 'dir';
        walk(abs);
      } else out[rel] = crypto.createHash('sha256').update(fs.readFileSync(abs)).digest('hex');
    }
  };
  walk(dir);
  return out;
}

/** The member names of `type`, sorted. */
function members(checker: ts.TypeChecker, type: ts.Type): string[] {
  return type.getProperties().map((p) => p.name).sort();
}

describe('carrick#1770: declarations for sources outside rootDir land in the stub', () => {
  let base: string;
  let repo: string;
  let before_: Record<string, string>;
  let result: ReturnType<typeof captureStub>;

  before(() => {
    base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1770-'));
    repo = path.join(base, 'repo');
    for (const [rel, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true });
      fs.writeFileSync(path.join(repo, rel), text);
    }
    before_ = snapshot(repo);
    result = captureStub({
      repoRoot: path.join(repo, 'apps/web'),
      serviceName: 'web',
      outDir: path.join(base, 'stub'),
      anchors: [
        {
          kind: 'symbol',
          alias: 'Endpoint_web_Response',
          symbol_name: 'WebOrder',
          source_file: 'src/routes.ts',
          anchor_origin: 'llm-symbol',
        },
        {
          kind: 'handler_return',
          alias: 'Endpoint_latest_Response',
          symbol_name: 'latest',
          source_file: 'src/routes.ts',
          anchor_origin: 'llm-symbol',
        },
        {
          kind: 'handler_return',
          alias: 'Endpoint_created_Response',
          symbol_name: 'created',
          source_file: 'src/routes.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
  });

  after(() => {
    fs.rmSync(base, { recursive: true, force: true });
  });

  it('captures, and every emitted file is inside the stub', () => {
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    // `emitted_files` are stub-relative, under `types/`.
    const escaped = result.emitted_files.filter(
      (rel) => !rel.startsWith('types/') || path.isAbsolute(rel) || rel.split('/').includes('..')
    );
    assert.deepStrictEqual(escaped, []);
    for (const rel of result.emitted_files) {
      assert.ok(fs.existsSync(path.join(result.stub_dir, rel)), `${rel} is not in the stub`);
    }
    assert.ok(
      result.emitted_files.some((rel) => rel.endsWith('schemas/order.d.ts')),
      `the sibling package's declaration is missing: ${result.emitted_files.join(', ')}`
    );
  });

  it('self-checks every alias clean', () => {
    assert.deepStrictEqual(
      result.aliases.map((a) => [a.alias, a.self_check]),
      [
        ['Endpoint_web_Response', 'ok'],
        ['Endpoint_latest_Response', 'ok'],
        ['Endpoint_created_Response', 'ok'],
      ],
      JSON.stringify(result.aliases)
    );
  });

  it('stands alone: a copy of the stub type-checks and reads the sibling types', () => {
    const copy = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1770-copy-'));
    try {
      fs.cpSync(path.join(result.stub_dir, 'types'), path.join(copy, 'types'), { recursive: true });
      const surface = path.join(copy, 'types', 'surface.d.ts');
      const program = ts.createProgram([surface], {
        strict: true,
        noEmit: true,
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.ESNext,
        moduleResolution: ts.ModuleResolutionKind.Bundler,
        types: [],
      });
      const diagnostics = ts
        .getPreEmitDiagnostics(program)
        .map((d) => `${d.file ? path.relative(copy, d.file.fileName) : ''}: ${ts.flattenDiagnosticMessageText(d.messageText, ' ')}`);
      assert.deepStrictEqual(diagnostics, []);
      // Nothing in the copy reaches back to the repo.
      const outside = program
        .getSourceFiles()
        .filter((f) => !program.isSourceFileDefaultLibrary(f) && !f.fileName.startsWith(copy));
      assert.deepStrictEqual(outside.map((f) => f.fileName), []);

      const checker = program.getTypeChecker();
      const source = program.getSourceFile(surface)!;
      const alias = (name: string) => {
        const decl = source.statements.find(
          (s): s is ts.TypeAliasDeclaration => ts.isTypeAliasDeclaration(s) && s.name.text === name
        )!;
        return checker.getTypeAtLocation(decl);
      };
      const web = alias('Endpoint_web_Response');
      const member = (type: ts.Type, name: string) =>
        checker.getTypeOfSymbol(checker.getPropertyOfType(type, name)!);
      assert.deepStrictEqual(members(checker, web), ['order', 'refund']);
      assert.deepStrictEqual(members(checker, member(web, 'order')), ['id', 'total']);
      assert.deepStrictEqual(members(checker, member(member(web, 'order'), 'total')), ['amount', 'currency']);
      assert.deepStrictEqual(members(checker, member(web, 'refund')), ['amount', 'currency']);
      const latest = alias('Endpoint_latest_Response');
      assert.deepStrictEqual(members(checker, latest), ['count', 'first']);
      assert.deepStrictEqual(members(checker, member(latest, 'first')), ['id', 'total']);
      const created = alias('Endpoint_created_Response');
      assert.deepStrictEqual(members(checker, member(created, 'id')), ['value']);
    } finally {
      fs.rmSync(copy, { recursive: true, force: true });
    }
  });

  it('leaves the repo byte-identical', () => {
    assert.deepStrictEqual(snapshot(repo), before_);
  });
});
