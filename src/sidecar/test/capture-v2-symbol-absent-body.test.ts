/**
 * A symbol anchor carries the absent-body-tolerated mark (carrick#2814).
 *
 * A route whose body type the model named is published by a symbol anchor,
 * not a literal one. The mark the inferrer set for that alias must reach the
 * alias's record, or the route keeps the incompatible reading against a sender
 * that may send nothing.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

describe('capture_v2: a symbol anchor carries absent_body_tolerated', () => {
  let root: string;
  let stub: CaptureStubResult;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-symbol-absent-'));
    const repoRoot = path.join(root, 'jobs-repo');
    fs.mkdirSync(path.join(repoRoot, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoRoot, 'src', 'types.ts'),
      'export interface ListOptions { location?: string; retries?: number }\n'
    );
    stub = captureStub({
      repoRoot,
      serviceName: 'jobs',
      outDir: path.join(root, 'jobs-stub'),
      anchors: [
        {
          kind: 'symbol',
          alias: 'Marked_Route',
          symbol_name: 'ListOptions',
          source_file: 'src/types.ts',
          anchor_origin: 'llm-symbol',
          absent_body_tolerated: true,
        },
        {
          kind: 'symbol',
          alias: 'Plain_Route',
          symbol_name: 'ListOptions',
          source_file: 'src/types.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.strictEqual(stub.success, true, JSON.stringify(stub.errors));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('the record carries the mark on the marked alias only', () => {
    assert.strictEqual(
      stub.aliases.find((a) => a.alias === 'Marked_Route')?.absent_body_tolerated,
      true
    );
    assert.strictEqual(
      stub.aliases.find((a) => a.alias === 'Plain_Route')?.absent_body_tolerated,
      undefined
    );
  });

  it('a sender that may send nothing is compatible with the marked alias only', async () => {
    const consumerRepo = path.join(root, 'panel-repo');
    fs.mkdirSync(consumerRepo, { recursive: true });
    const consumer = captureStub({
      repoRoot: consumerRepo,
      serviceName: 'panel',
      outDir: path.join(root, 'panel-stub'),
      anchors: [
        {
          kind: 'literal',
          alias: 'Maybe',
          type_text: '{ location?: string; } | undefined',
          anchor_origin: 'deterministic-infer',
        },
      ],
    });
    assert.strictEqual(consumer.success, true, JSON.stringify(consumer.errors));
    const result = await runCheck({
      stubs: [
        { service_name: 'jobs', stub_dir: stub.stub_dir },
        { service_name: 'panel', stub_dir: consumer.stub_dir },
      ],
      pairs: ['Marked_Route', 'Plain_Route'].map((alias) => ({
        pair_key: alias,
        protocol: 'http' as const,
        type_kind: 'request' as const,
        producer: { service_name: 'jobs', alias },
        consumer: { service_name: 'panel', alias: 'Maybe' },
      })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    const bucket = (key: string) => result.verdicts.find((v) => v.pair_key === key)?.bucket;
    assert.strictEqual(bucket('Marked_Route'), 'compatible');
    assert.notStrictEqual(bucket('Plain_Route'), 'compatible');
  });
});
