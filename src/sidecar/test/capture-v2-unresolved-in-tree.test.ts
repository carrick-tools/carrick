/**
 * carrick#1446: a capture record reads clean for an alias whose own type does
 * not resolve in the emitted tree.
 *
 * A literal anchor's text names types as they read where v1 printed it. A
 * name its source module declares but does not export cannot be imported into
 * the surface, so the surface names it bare and the compiler reads that
 * position as its unresolved-reference placeholder: `any`, printed as the
 * name. The program declares the name, so `undeclared_names` stays empty; the
 * deep walk leaves the placeholder out on purpose (a pinned external heals at
 * check time); the record said `self_check: ok` with nothing else, and every
 * reader of the record counted the type clean.
 *
 * The record now lists, in `unresolved_in_tree`, every position at which the
 * alias's type holds that placeholder in the emitted tree: `''` for the alias's
 * own type, member paths otherwise. It is a list of its own, not part of
 * `any_provenance`, because the check phase pre-gates on `any_provenance[0]`
 * and this record is about what the index can publish, not about a verdict.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import type { CaptureAliasRecord, CaptureStubResult } from '../src/capture/api.js';

function writeRepo(root: string, files: Record<string, string>): string {
  fs.mkdirSync(root, { recursive: true });
  for (const [name, text] of Object.entries(files)) {
    const p = path.join(root, name);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, text);
  }
  return root;
}

const literal = (alias: string, type_text: string, source_file: string) => ({
  kind: 'literal' as const,
  alias,
  type_text,
  anchor_origin: 'deterministic-infer' as const,
  source_file,
});

function recordOf(result: CaptureStubResult, alias: string): CaptureAliasRecord {
  const record = result.aliases.find((a) => a.alias === alias);
  assert.ok(record, `no record for ${alias}: ${JSON.stringify(result.aliases)}`);
  return record;
}

describe('the capture record names what does not resolve in the emitted tree (#1446)', () => {
  let root: string;
  let result: CaptureStubResult;

  before(() => {
    // Realpath: macOS's tmpdir is a symlink (see the #1774 test).
    root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1446-')));
    const repoRoot = writeRepo(path.join(root, 'repo'), {
      'tsconfig.json': JSON.stringify({
        compilerOptions: {
          strict: true,
          target: 'es2022',
          module: 'esnext',
          moduleResolution: 'bundler',
        },
        include: ['src'],
      }),
      'src/api.ts': [
        '// Declared here, never exported: no other module can import it.',
        'interface Task { id: string; done: boolean; }',
        'export interface Summary { total: number; }',
        'export function tasks(): Task[] { return []; }',
        '',
      ].join('\n'),
    });
    result = captureStub({
      repoRoot,
      serviceName: 'unresolved-in-tree',
      outDir: path.join(root, 'stub'),
      anchors: [
        literal('L_Member', '{ tasks: Task[]; count: number; }', 'src/api.ts'),
        literal('L_Root', 'Task | null', 'src/api.ts'),
        literal('L_Clean', '{ summary: Summary; }', 'src/api.ts'),
        literal('L_DeclaredAny', '{ meta: any; id: string; }', 'src/api.ts'),
        literal('L_RootAny', 'any', 'src/api.ts'),
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('a member naming a type the surface cannot see is listed at its path, and the record stays ok', () => {
    const record = recordOf(result, 'L_Member');
    // What the record said before: clean on every field a reader checks.
    assert.strictEqual(record.self_check, 'ok', record.self_check_detail);
    assert.strictEqual(record.top_type_at_self_check, false);
    assert.strictEqual(record.any_provenance, undefined, 'the pre-gate list is untouched');
    assert.strictEqual(record.undeclared_names, undefined, 'the program declares the name');

    assert.deepStrictEqual(
      record.unresolved_in_tree?.map(({ path, kind, reason }) => ({ path, kind, reason })),
      [{ path: 'tasks<0>', kind: 'any', reason: 'unresolved_import' }]
    );
    assert.match(record.unresolved_in_tree![0].detail ?? '', /'Task'/, 'the detail names it');
  });

  it("an alias whose own type does not resolve is listed at its root ('')", () => {
    const record = recordOf(result, 'L_Root');
    assert.strictEqual(record.self_check, 'decayed_internal');
    assert.strictEqual(record.top_type_at_self_check, true);
    assert.deepStrictEqual(
      record.unresolved_in_tree?.map(({ path, kind, reason }) => ({ path, kind, reason })),
      [{ path: '', kind: 'any', reason: 'unresolved_import' }]
    );
    assert.match(record.unresolved_in_tree![0].detail ?? '', /'Task'/);
  });

  it('a type that resolves carries no list', () => {
    const record = recordOf(result, 'L_Clean');
    assert.strictEqual(record.self_check, 'ok', record.self_check_detail);
    assert.strictEqual(record.unresolved_in_tree, undefined);
  });

  it("an author's any is a declared finding, not an unresolved one", () => {
    const member = recordOf(result, 'L_DeclaredAny');
    assert.deepStrictEqual(
      member.any_provenance?.map(({ path, reason }) => ({ path, reason })),
      [{ path: 'meta', reason: 'declared' }]
    );
    assert.strictEqual(member.unresolved_in_tree, undefined);

    const rootAny = recordOf(result, 'L_RootAny');
    assert.strictEqual(rootAny.top_type_at_self_check, true);
    assert.strictEqual(rootAny.unresolved_in_tree, undefined);
  });

  it('the list does not pre-gate a pair: the check reads any_provenance only', async () => {
    const consumer = captureStub({
      repoRoot: writeRepo(path.join(root, 'consumer'), {}),
      serviceName: 'unresolved-consumer',
      outDir: path.join(root, 'consumer-stub'),
      anchors: [
        {
          kind: 'literal',
          alias: 'C_Member',
          type_text: '{ tasks: { id: string; done: boolean; }[]; count: number; }',
          anchor_origin: 'deterministic-infer',
        },
      ],
    });
    assert.ok(consumer.success, JSON.stringify(consumer.errors));
    const check = await runCheck({
      stubs: [
        { service_name: 'unresolved-in-tree', stub_dir: result.stub_dir },
        { service_name: 'unresolved-consumer', stub_dir: consumer.stub_dir },
      ],
      pairs: [
        {
          pair_key: 'member-pair',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'unresolved-in-tree', alias: 'L_Member' },
          consumer: { service_name: 'unresolved-consumer', alias: 'C_Member' },
        },
      ],
    });
    assert.strictEqual(check.success, true, JSON.stringify(check.errors));
    const verdict = check.verdicts.find((v) => v.pair_key === 'member-pair');
    assert.ok(verdict, JSON.stringify(check.verdicts));
    assert.ok(!(verdict.gate ?? '').startsWith('capture:'), JSON.stringify(verdict));
  });
});

/** An ambient stub whose `export =` hides FastifyInstance, so a file that
 * default-exports an instance hits TS4023 and its declaration emit is skipped
 * (the shape `capture-v2-partial-emit.test.ts` pins). */
const FASTIFY_STUB = `declare module "fastify" {
  interface FastifyInstance {
    get(path: string, handler: () => void): this;
  }
  function fastify(): FastifyInstance;
  export = fastify;
}
`;

describe('a literal whose module was not emitted is listed at its root (#1446, #1165)', () => {
  let root: string;
  let result: CaptureStubResult;

  before(() => {
    root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1446-emit-')));
    const repoRoot = writeRepo(path.join(root, 'repo'), {
      'tsconfig.json': JSON.stringify({
        compilerOptions: {
          target: 'ES2020',
          module: 'commonjs',
          strict: true,
          esModuleInterop: true,
          skipLibCheck: true,
          declaration: true,
          rootDir: './',
        },
        include: ['**/*.ts'],
      }),
      'src/types/stubs.d.ts': FASTIFY_STUB,
      'src/http/routes.ts':
        'import fastify from "fastify";\n' +
        'const app = fastify();\n' +
        'export interface RouteReply { id: string; message: string; }\n' +
        'export default app;\n',
    });
    result = captureStub({
      repoRoot,
      serviceName: 'emit-skipped',
      outDir: path.join(root, 'stub'),
      anchors: [
        // The text names an export of the module whose emit is skipped: the
        // capture imports it from there, and the import dangles.
        literal('L_Replies', 'RouteReply[]', 'src/http/routes.ts'),
        {
          kind: 'symbol',
          alias: 'S_Reply',
          symbol_name: 'RouteReply',
          source_file: 'src/http/routes.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('the demoted literal says its own type does not resolve', () => {
    const record = recordOf(result, 'L_Replies');
    assert.match(record.capture_failure_reason ?? '', /declaration emit was skipped for module/);
    assert.deepStrictEqual(
      record.unresolved_in_tree?.map(({ path, kind, reason }) => ({ path, kind, reason })),
      [{ path: '', kind: 'any', reason: 'unresolved_import' }]
    );
    const detail = record.unresolved_in_tree![0].detail ?? '';
    assert.ok(!detail.includes(root), `the detail names no machine path: ${detail}`);
  });

  it('a demoted symbol anchor says nothing: the bundle states its declaration, not this text', () => {
    const record = recordOf(result, 'S_Reply');
    assert.match(record.capture_failure_reason ?? '', /declaration emit was skipped for module/);
    assert.strictEqual(record.unresolved_in_tree, undefined);
  });
});
