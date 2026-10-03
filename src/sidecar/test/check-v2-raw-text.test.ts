/**
 * carrick#1842: a consumer that reads the body as raw text states no
 * structural contract, so the pair is unverifiable, as a body of bytes is
 * (carrick#1812).
 *
 * The capture records the inferrer's mark on the alias (`raw_text_read`), the
 * stub carries it to every scan that checks against this service, and the
 * judge gates on it beside the bytes gate, so it holds whether the sides
 * agree or not. A `string` consumer with no mark is still compared, and a
 * side that is `any` keeps its own reason.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import type { CaptureStubResult, CheckPairSpec, CheckVerdict } from '../src/capture/api.js';

function captureLiterals(
  root: string,
  serviceName: string,
  literals: Record<string, { text: string; rawText?: boolean }>
): CaptureStubResult {
  const repoRoot = path.join(root, `${serviceName}-repo`);
  fs.mkdirSync(repoRoot, { recursive: true });
  const result = captureStub({
    repoRoot,
    serviceName,
    outDir: path.join(root, `${serviceName}-stub`),
    anchors: Object.entries(literals).map(([alias, { text, rawText }]) => ({
      kind: 'literal' as const,
      alias,
      type_text: text,
      anchor_origin: 'deterministic-infer' as const,
      ...(rawText ? { raw_text_read: true as const } : {}),
    })),
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return result;
}

describe('check phase: a consumer that reads raw text states no contract (carrick#1842)', () => {
  let root: string;
  let producer: CaptureStubResult;
  let consumer: CaptureStubResult;
  let verdicts: Map<string, CheckVerdict>;

  const pair = (
    pair_key: string,
    producerAlias: string,
    consumerAlias: string,
    over: Partial<CheckPairSpec> = {}
  ): CheckPairSpec => ({
    pair_key,
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'text-producer', alias: producerAlias },
    consumer: { service_name: 'text-consumer', alias: consumerAlias },
    ...over,
  });

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1842-check-'));
    producer = captureLiterals(root, 'text-producer', {
      P_Object: { text: '{ ok: true }' },
      P_Object2: { text: '{ ok: true }' },
      P_String: { text: 'string' },
      P_Any: { text: 'any' },
      P_Bytes: { text: 'Uint8Array' },
    });
    consumer = captureLiterals(root, 'text-consumer', {
      C_TextVsObject: { text: 'string', rawText: true },
      C_TextVsString: { text: 'string', rawText: true },
      C_TextVsAny: { text: 'string', rawText: true },
      C_TextVsBytes: { text: 'string', rawText: true },
      C_TextPubsub: { text: 'string', rawText: true },
      C_PlainString: { text: 'string' },
    });
    const result = await runCheck({
      stubs: [
        { service_name: 'text-producer', stub_dir: producer.stub_dir },
        { service_name: 'text-consumer', stub_dir: consumer.stub_dir },
      ],
      pairs: [
        pair('text-object', 'P_Object', 'C_TextVsObject'),
        pair('text-string', 'P_String', 'C_TextVsString'),
        pair('text-any', 'P_Any', 'C_TextVsAny'),
        pair('text-bytes', 'P_Bytes', 'C_TextVsBytes'),
        pair('text-pubsub', 'P_Object2', 'C_TextPubsub', { protocol: 'pubsub', type_kind: 'both' }),
        pair('plain-string', 'P_Object', 'C_PlainString'),
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    verdicts = new Map(result.verdicts.map((v) => [v.pair_key, v]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('the capture record carries the mark, and only on the marked alias', () => {
    const marked = consumer.aliases.find((a) => a.alias === 'C_TextVsObject');
    const plain = consumer.aliases.find((a) => a.alias === 'C_PlainString');
    assert.strictEqual(marked?.raw_text_read, true);
    assert.strictEqual(plain?.raw_text_read, undefined);
    const stubRecords = JSON.parse(
      fs.readFileSync(path.join(consumer.stub_dir, 'carrick-manifest.json'), 'utf8')
    ) as { aliases: Array<{ alias: string; raw_text_read?: boolean }> };
    assert.strictEqual(
      stubRecords.aliases.find((a) => a.alias === 'C_TextVsObject')?.raw_text_read,
      true,
      'the stub every peer scan reads carries the mark'
    );
  });

  it('a text read against an object body is unverifiable, not incompatible', () => {
    const v = verdicts.get('text-object')!;
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'consumer:text');
    assert.strictEqual(v.resolved, false);
    assert.strictEqual(v.unresolved_side, 'consumer');
    assert.match(v.diagnostic!, /raw text/);
  });

  it('a text read against a string body is unverifiable too, as agreeing bytes are', () => {
    const v = verdicts.get('text-string')!;
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
    assert.strictEqual(v.gate, 'consumer:text');
  });

  it('a top type keeps its own reason, and bytes keep theirs', () => {
    const any = verdicts.get('text-any')!;
    assert.strictEqual(any.bucket, 'gate_caught_baked_any', JSON.stringify(any));
    assert.strictEqual(any.gate, 'producer:any');
    const bytes = verdicts.get('text-bytes')!;
    assert.strictEqual(bytes.bucket, 'unverifiable', JSON.stringify(bytes));
    assert.strictEqual(bytes.gate, 'producer:bytes');
  });

  it('the mark gates an http body only', () => {
    const v = verdicts.get('text-pubsub')!;
    assert.notStrictEqual(v.gate, 'consumer:text', JSON.stringify(v));
    assert.notStrictEqual(v.gate, 'producer:text', JSON.stringify(v));
  });

  it('an unmarked string consumer is still compared', () => {
    const v = verdicts.get('plain-string')!;
    assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
  });
});
