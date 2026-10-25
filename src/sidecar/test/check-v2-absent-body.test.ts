/**
 * An absent sent body against a receiver that requires none (carrick#2814).
 *
 * A body the sender may omit (`T | undefined`, `T | void`) is compatible only
 * when the receiver's type has no required member AND either its type admits
 * absence, or its handler works with nothing sent (the capture record's
 * `absent_body_tolerated`, from the inferrer). A handler with no mark, the
 * one that destructures its body or is out of reach, keeps the incompatible
 * reading: a server library hands it `undefined`, and it throws. The members
 * the sender does send are still compared. Real pnpm + tsc.
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
  literals: Record<string, { text: string; tolerated?: boolean }>
): CaptureStubResult {
  const repoRoot = path.join(root, `${serviceName}-repo`);
  fs.mkdirSync(repoRoot, { recursive: true });
  const result = captureStub({
    repoRoot,
    serviceName,
    outDir: path.join(root, `${serviceName}-stub`),
    anchors: Object.entries(literals).map(([alias, { text, tolerated }]) => ({
      kind: 'literal' as const,
      alias,
      type_text: text,
      anchor_origin: 'deterministic-infer' as const,
      ...(tolerated ? { absent_body_tolerated: true as const } : {}),
    })),
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return result;
}

function mk(
  pair_key: string,
  producerAlias: string,
  consumerAlias: string,
  over: Partial<CheckPairSpec> = {}
): CheckPairSpec {
  return {
    pair_key,
    protocol: 'http',
    type_kind: 'request',
    producer: { service_name: 'jobs', alias: producerAlias },
    consumer: { service_name: 'panel', alias: consumerAlias },
    ...over,
  };
}

const PAIRS: CheckPairSpec[] = [
  // The handler is in reach and reads the body only through `?.` / `?? {}`.
  mk('tolerant_undefined', 'Tolerant_Route', 'Opt_Maybe'),
  mk('tolerant_void', 'Tolerant_Route', 'Opt_Void'),
  mk('tolerant_index', 'Tolerant_Index_Route', 'Opt_Maybe'),
  mk('tolerant_wrong_member', 'Tolerant_Route', 'Opt_Wrong_Maybe'),
  mk('tolerant_required', 'Tolerant_Required_Route', 'Req_Maybe'),
  mk('tolerant_scalar', 'Tolerant_Text_Route', 'Text_Maybe'),
  // The handler destructures its body, or is out of reach: no mark.
  mk('unmarked_undefined', 'Unmarked_Route', 'Opt_Maybe'),
  mk('unmarked_void', 'Unmarked_Route', 'Opt_Void'),
  mk('unmarked_present', 'Unmarked_Route', 'Opt_Plain'),
  // The route's own type admits absence.
  mk('declared_absent_void', 'Declared_Absent_Route', 'Opt_Void'),
  // A subscriber's payload.
  mk('socket_tolerant', 'Tolerant_Route', 'Opt_Maybe', { protocol: 'socket', type_kind: 'both' }),
  mk('socket_unmarked', 'Unmarked_Route', 'Opt_Maybe', { protocol: 'socket', type_kind: 'both' }),
  mk('pubsub_tolerant', 'Tolerant_Route', 'Opt_Maybe', { protocol: 'pubsub', type_kind: 'both' }),
];

describe('check_v2: an absent sent body (carrick#2814)', () => {
  let root: string;
  let verdicts: Map<string, CheckVerdict>;
  let producer: CaptureStubResult;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-absent-'));
    producer = captureLiterals(root, 'jobs', {
      Tolerant_Route: { text: '{ location?: string; retries?: number; }', tolerated: true },
      Tolerant_Index_Route: { text: '{ [key: string]: string; }', tolerated: true },
      Tolerant_Required_Route: { text: '{ name: string; location?: string; }', tolerated: true },
      Tolerant_Text_Route: { text: 'string', tolerated: true },
      Unmarked_Route: { text: '{ location?: string; retries?: number; }' },
      Declared_Absent_Route: { text: '{ location?: string; } | undefined' },
    });
    const consumer = captureLiterals(root, 'panel', {
      Opt_Maybe: { text: '{ location?: string; } | undefined' },
      Opt_Void: { text: '{ location?: string; } | void' },
      Opt_Plain: { text: '{ location?: string; }' },
      Opt_Wrong_Maybe: { text: '{ location?: number; } | undefined' },
      Req_Maybe: { text: '{ name: string; } | undefined' },
      Text_Maybe: { text: 'string | undefined' },
    });
    const result = await runCheck({
      stubs: [
        { service_name: 'jobs', stub_dir: producer.stub_dir },
        { service_name: 'panel', stub_dir: consumer.stub_dir },
      ],
      pairs: PAIRS,
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    verdicts = new Map(result.verdicts.map((v) => [v.pair_key, v]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const bucket = (key: string): string => {
    const v = verdicts.get(key)!;
    return `${v.bucket}${v.diagnostic ? `: ${v.diagnostic}` : ''}`;
  };

  it('the capture record carries the mark, and only on the marked alias', () => {
    assert.strictEqual(producer.aliases.find((a) => a.alias === 'Tolerant_Route')?.absent_body_tolerated, true);
    assert.strictEqual(producer.aliases.find((a) => a.alias === 'Unmarked_Route')?.absent_body_tolerated, undefined);
  });

  it('a handler that tolerates no body accepts an absent body when its type requires no member', () => {
    for (const key of ['tolerant_undefined', 'tolerant_void', 'tolerant_index']) {
      assert.match(bucket(key), /^compatible/, key);
    }
  });

  it('the members it does send are still compared', () => {
    assert.match(bucket('tolerant_wrong_member'), /^incompatible/);
  });

  it('a type with a required member, or a scalar, keeps the incompatible verdict', () => {
    for (const key of ['tolerant_required', 'tolerant_scalar']) {
      assert.match(bucket(key), /^incompatible/, key);
    }
  });

  it('a handler that dereferences its body, or is out of reach, keeps the incompatible verdict', () => {
    for (const key of ['unmarked_undefined', 'unmarked_void']) {
      assert.match(bucket(key), /^incompatible/, key);
    }
    assert.match(bucket('unmarked_present'), /^compatible/);
  });

  it('a type that admits absence accepts an absent body with no mark', () => {
    assert.match(bucket('declared_absent_void'), /^compatible/);
  });

  it('a socket or pub/sub subscriber is read the same way', () => {
    assert.match(bucket('socket_tolerant'), /^compatible/);
    assert.match(bucket('pubsub_tolerant'), /^compatible/);
    assert.match(bucket('socket_unmarked'), /^incompatible/);
  });
});
