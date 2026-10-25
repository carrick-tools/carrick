/**
 * An absent caller body against a route that does not require one
 * (carrick#2814). A request body the caller may omit (`T | undefined`,
 * `T | void`) is compatible with a route whose request type has no required
 * member; the members it does send are still compared. A route that requires
 * a member keeps the incompatible verdict. Real pnpm + tsc.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import type { CheckPairSpec, CheckVerdict } from '../src/capture/api.js';

function writeStub(dir: string, serviceName: string, surface: string): void {
  const stubDir = path.join(dir, serviceName);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify({
      name: `@carrick/${serviceName}`,
      version: '0.0.0-carrick',
      private: true,
      types: './types/surface.d.ts',
    }) + '\n'
  );
  fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
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
  mk('optional_undefined', 'Opt_Route', 'Opt_Maybe'),
  mk('optional_void', 'Opt_Route', 'Opt_Void'),
  mk('optional_present', 'Opt_Route', 'Opt_Plain'),
  mk('optional_wrong_member', 'Opt_Route', 'Opt_Wrong_Maybe'),
  mk('required_undefined', 'Req_Route', 'Req_Maybe'),
  mk('required_void', 'Req_Route', 'Req_Void'),
  mk('index_undefined', 'Index_Route', 'Opt_Maybe'),
  mk('scalar_undefined', 'Text_Route', 'Text_Maybe'),
  // Not reached: a socket payload keeps today's reading.
  mk('socket_undefined', 'Opt_Route', 'Opt_Maybe', { protocol: 'socket', type_kind: 'both' }),
];

describe('check_v2: an absent caller body (carrick#2814)', () => {
  let root: string;
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-absent-'));
    writeStub(
      root,
      'jobs',
      [
        'export type Opt_Route = { location?: string; retries?: number; };',
        'export type Req_Route = { name: string; location?: string; };',
        'export type Index_Route = { [key: string]: string; };',
        'export type Text_Route = string;',
      ].join('\n') + '\n'
    );
    writeStub(
      root,
      'panel',
      [
        'export type Opt_Maybe = { location?: string; } | undefined;',
        'export type Opt_Void = { location?: string; } | void;',
        'export type Opt_Plain = { location?: string; };',
        'export type Opt_Wrong_Maybe = { location?: number; } | undefined;',
        'export type Req_Maybe = { name: string; } | undefined;',
        'export type Req_Void = { name: string; } | void;',
        'export type Text_Maybe = string | undefined;',
      ].join('\n') + '\n'
    );
    const result = await runCheck({
      stubs: [
        { service_name: 'jobs', stub_dir: path.join(root, 'jobs') },
        { service_name: 'panel', stub_dir: path.join(root, 'panel') },
      ],
      pairs: PAIRS,
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    verdicts = new Map(result.verdicts.map((v) => [v.pair_key, v]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('an absent body is compatible with a route that has no required member', () => {
    for (const key of ['optional_undefined', 'optional_void', 'optional_present', 'index_undefined']) {
      const v = verdicts.get(key)!;
      assert.strictEqual(v.bucket, 'compatible', `${key}: ${v.diagnostic}`);
    }
  });

  it('the members it does send are still compared', () => {
    assert.strictEqual(verdicts.get('optional_wrong_member')!.bucket, 'incompatible');
  });

  it('a route that requires a member keeps the incompatible verdict', () => {
    for (const key of ['required_undefined', 'required_void', 'scalar_undefined']) {
      assert.strictEqual(verdicts.get(key)!.bucket, 'incompatible', key);
    }
  });

  it('a socket payload is not reached', () => {
    assert.strictEqual(verdicts.get('socket_undefined')!.bucket, 'incompatible');
  });
});
