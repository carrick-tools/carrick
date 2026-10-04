/**
 * Partially skipped declaration emit — the corpus-2 notifications-svc
 * collapse.
 *
 * `program.emit` sets `emitSkipped` per-PROGRAM even when only one file's
 * declaration emit failed: an ambient `declare module "fastify"` stub with
 * `export =` and no `FastifyInstance` export makes `export default app`
 * unnameable (TS4023), that one file's .d.ts is skipped, and every other
 * file's .d.ts is still written to the emit callback. The old wholesale
 * `if (emitSkipped) fail` threw the entire emitted tree away, so the whole
 * service's types went unresolved and its pub/sub edges collapsed to None.
 *
 * Pins:
 *  1. capture keeps the emitted subset: success, healthy aliases fully
 *     resolved, the tree contains their .d.ts files;
 *  2. an alias whose module produced no .d.ts is demoted to
 *     structural_fallback with a capture_failure_reason, and its surface
 *     line is rewritten to `unknown` (no dangling specifier that would
 *     smear healthy aliases at self-check or poison the service at check);
 *  3. the total-failure path is retained: nothing emitted at all is still
 *     the wholesale 'declaration emit was skipped' fail;
 *  4. fail-closed at check: a demoted producer alias verdicts unverifiable
 *     (IsUnknown probe gate), never compatible, while a healthy alias from
 *     the SAME partially-emitted capture still probes compatible.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { captureStub, runCheck } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// dist/test -> dist -> sidecar -> src -> repo root.
const CORPUS2_NOTIFICATIONS = path.join(
  __dirname, '..', '..', '..', '..',
  'tests', 'fixtures', 'xrepo-corpus-2', 'notifications-svc'
);

/** Ambient stub whose `export =` hides FastifyInstance: `export default app`
 * in a file importing it hits TS4023 and that file's declaration emit is
 * skipped. Mirrors tests/fixtures/xrepo-corpus-2/notifications-svc. */
const FASTIFY_STUB = `declare module "fastify" {
  interface FastifyInstance {
    get(path: string, handler: () => void): this;
  }
  function fastify(): FastifyInstance;
  export = fastify;
}
`;

const TSCONFIG = JSON.stringify({
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
});

function writeRepo(root: string, files: Record<string, string>): string {
  fs.mkdirSync(root, { recursive: true });
  for (const [name, text] of Object.entries(files)) {
    const p = path.join(root, name);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, text);
  }
  return root;
}

/** The corpus-2 shape: a TS4023-poisoned routes file + a clean events file. */
function capturePartialRepo(root: string): CaptureStubResult {
  const repoRoot = writeRepo(path.join(root, 'partial-repo'), {
    'tsconfig.json': TSCONFIG,
    'src/types/stubs.d.ts': FASTIFY_STUB,
    'src/http/routes.ts':
      'import fastify from "fastify";\n' +
      'const app = fastify();\n' +
      'export interface RouteReply { id: string; message: string; }\n' +
      'export default app;\n',
    'src/types/events.ts':
      'export interface OrderPlacedEvent {\n' +
      '  id: string;\n' +
      '  total: { amountCents: number; currency: string };\n' +
      '}\n',
  });
  return captureStub({
    repoRoot,
    serviceName: 'partial-emit-svc',
    outDir: path.join(root, 'partial-stub'),
    anchors: [
      {
        kind: 'symbol',
        alias: 'P_Event',
        symbol_name: 'OrderPlacedEvent',
        source_file: 'src/types/events.ts',
        anchor_origin: 'llm-symbol',
      },
      {
        kind: 'symbol',
        alias: 'P_RouteReply',
        symbol_name: 'RouteReply',
        source_file: 'src/http/routes.ts',
        anchor_origin: 'llm-symbol',
      },
    ],
  });
}

describe('capture v2: partially skipped declaration emit keeps the emitted subset', () => {
  let root: string;
  let result: CaptureStubResult;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-partial-emit-'));
    result = capturePartialRepo(root);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('capture succeeds instead of failing wholesale', () => {
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    // The TS4023 diagnostic and the partial-emit note ride the errors channel
    // so the scanner can see the degradation.
    assert.ok(
      result.errors.some((e) => /cannot be named/.test(e)),
      JSON.stringify(result.errors)
    );
    assert.ok(
      result.errors.some((e) => /declaration emit was partial/.test(e)),
      JSON.stringify(result.errors)
    );
  });

  it('the emitted tree contains the healthy events .d.ts and not the skipped module', () => {
    assert.ok(
      result.emitted_files.includes('types/src/types/events.d.ts'),
      JSON.stringify(result.emitted_files)
    );
    assert.ok(
      !result.emitted_files.includes('types/src/http/routes.d.ts'),
      JSON.stringify(result.emitted_files)
    );
  });

  it('the events alias is fully resolved and usable', () => {
    const event = result.aliases.find((a) => a.alias === 'P_Event')!;
    assert.strictEqual(event.serialization, 'emitted');
    assert.strictEqual(event.self_check, 'ok', event.self_check_detail);
    assert.strictEqual(event.capture_failure_reason, undefined);
  });

  it('the routes alias is demoted with a recorded capture_failure_reason', () => {
    const reply = result.aliases.find((a) => a.alias === 'P_RouteReply')!;
    assert.strictEqual(reply.serialization, 'structural_fallback');
    assert.strictEqual(reply.self_check, 'decayed_internal');
    assert.match(reply.capture_failure_reason ?? '', /declaration emit was skipped for module/);
  });

  it('the demoted surface line is `unknown`, never a dangling import', () => {
    const surface = fs.readFileSync(
      path.join(result.stub_dir, 'types', 'surface.d.ts'),
      'utf8'
    );
    assert.match(surface, /export type P_RouteReply = unknown;/);
    assert.ok(!surface.includes('src/http/routes'), surface);
    // The healthy alias keeps its real emitted reference.
    assert.match(surface, /export type P_Event = import\(.\.\/src\/types\/events.\)\.OrderPlacedEvent;/);
  });
});

describe('capture v2: total emit failure still fails wholesale', () => {
  let root: string;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-total-skip-'));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it("zero emitted files returns the 'declaration emit was skipped' fail", () => {
    // noEmitOnError + a syntax error blocks the WHOLE emit (syntactic
    // diagnostics survive noCheck), so not even the surface entry emits —
    // the true nothing-emitted shape the wholesale fail is reserved for.
    const repoRoot = writeRepo(path.join(root, 'total-repo'), {
      'tsconfig.json': JSON.stringify({
        compilerOptions: {
          target: 'ES2020',
          module: 'commonjs',
          strict: true,
          declaration: true,
          noEmitOnError: true,
          rootDir: './',
        },
        include: ['**/*.ts'],
      }),
      'src/broken.ts': 'export interface Order { id: string\nconst oops = ;\n',
    });
    const result = captureStub({
      repoRoot,
      serviceName: 'total-skip-svc',
      outDir: path.join(root, 'total-stub'),
      anchors: [
        {
          kind: 'symbol',
          alias: 'P_Order',
          symbol_name: 'Order',
          source_file: 'src/broken.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.strictEqual(result.success, false);
    assert.deepStrictEqual(result.errors, ['declaration emit was skipped']);
    assert.deepStrictEqual(result.aliases, []);
    assert.deepStrictEqual(result.emitted_files, []);
  });
});

describe('check v2: a partial-emit-demoted alias is never compatible', () => {
  let root: string;
  let producer: CaptureStubResult;
  let consumer: CaptureStubResult;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-partial-check-'));
    producer = capturePartialRepo(root);
    assert.strictEqual(producer.success, true, JSON.stringify(producer.errors));
    const consumerRepo = writeRepo(path.join(root, 'consumer-repo'), {});
    consumer = captureStub({
      repoRoot: consumerRepo,
      serviceName: 'partial-consumer',
      outDir: path.join(root, 'consumer-stub'),
      anchors: [
        {
          kind: 'literal',
          alias: 'C_Reply',
          type_text: '{ id: string; message: string }',
          anchor_origin: 'deterministic-infer',
        },
        {
          kind: 'literal',
          alias: 'C_Event',
          type_text: '{ id: string; total: { amountCents: number; currency: string } }',
          anchor_origin: 'deterministic-infer',
        },
      ],
    });
    assert.strictEqual(consumer.success, true, JSON.stringify(consumer.errors));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('demoted producer -> unverifiable (unknown gate); healthy sibling still compatible', async () => {
    const result = await runCheck({
      stubs: [
        { service_name: 'partial-emit-svc', stub_dir: producer.stub_dir },
        { service_name: 'partial-consumer', stub_dir: consumer.stub_dir },
      ],
      pairs: [
        {
          pair_key: 'demoted-pair',
          protocol: 'http',
          type_kind: 'response',
          producer: { service_name: 'partial-emit-svc', alias: 'P_RouteReply' },
          consumer: { service_name: 'partial-consumer', alias: 'C_Reply' },
        },
        {
          pair_key: 'healthy-pair',
          protocol: 'pubsub',
          type_kind: 'both',
          producer: { service_name: 'partial-emit-svc', alias: 'P_Event' },
          consumer: { service_name: 'partial-consumer', alias: 'C_Event' },
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const byKey = new Map(result.verdicts.map((v) => [v.pair_key, v]));

    // FAIL-CLOSED INVARIANT: the demoted alias must never read compatible.
    const demoted = byKey.get('demoted-pair')!;
    assert.notStrictEqual(demoted.bucket, 'compatible', JSON.stringify(demoted));
    assert.strictEqual(demoted.bucket, 'unverifiable', JSON.stringify(demoted));
    assert.strictEqual(demoted.gate, 'producer:unknown');

    // The recovery is real: the healthy alias from the SAME partial capture
    // still verifies (this also proves the kept tree carries no poison).
    const healthy = byKey.get('healthy-pair')!;
    assert.strictEqual(healthy.bucket, 'compatible', JSON.stringify(healthy));
  });
});

describe('capture v2: corpus-2 notifications-svc regression (repo fixture)', () => {
  const available = fs.existsSync(CORPUS2_NOTIFICATIONS);
  let outDir: string;

  before(() => {
    outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-notif-fixture-'));
  });

  after(() => {
    fs.rmSync(outDir, { recursive: true, force: true });
  });

  it('pub/sub contract aliases survive the routes-file TS4023', { skip: !available }, () => {
    const result = captureStub({
      repoRoot: CORPUS2_NOTIFICATIONS,
      serviceName: 'notifications-svc',
      outDir: path.join(outDir, 'stub'),
      anchors: [
        {
          kind: 'symbol',
          alias: 'Pub_OrderPlacedEvent',
          symbol_name: 'OrderPlacedEvent',
          source_file: 'src/types/events.ts',
          anchor_origin: 'llm-symbol',
        },
        {
          kind: 'symbol',
          alias: 'Pub_UserRegisteredEvent',
          symbol_name: 'UserRegisteredEvent',
          source_file: 'src/types/events.ts',
          anchor_origin: 'llm-symbol',
        },
        {
          kind: 'symbol',
          alias: 'Http_Notification',
          symbol_name: 'Notification',
          source_file: 'src/http/routes.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    // Pre-fix: success false, errors ['declaration emit was skipped'], zero
    // aliases — the whole service collapsed and its 4 pub/sub edges read None.
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.ok(result.emitted_files.includes('types/src/types/events.d.ts'));
    const byAlias = new Map(result.aliases.map((a) => [a.alias, a]));
    for (const alias of ['Pub_OrderPlacedEvent', 'Pub_UserRegisteredEvent']) {
      const rec = byAlias.get(alias)!;
      assert.strictEqual(rec.serialization, 'emitted', alias);
      assert.strictEqual(rec.self_check, 'ok', `${alias}: ${rec.self_check_detail}`);
    }
    const routes = byAlias.get('Http_Notification')!;
    assert.strictEqual(routes.serialization, 'structural_fallback');
    assert.match(routes.capture_failure_reason ?? '', /declaration emit was skipped for module/);
  });
});
