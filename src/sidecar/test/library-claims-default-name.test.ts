/**
 * A name slot typed by a type parameter with no constraint reads through its
 * default (T2b): `<T = string>(message: T)` takes a name string, `<T>(message: T)`
 * and `(message: unknown)` say nothing. Only the name, base and prefix check
 * reads the default. Every package below is invented.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import type { LibraryCheck } from '../src/types.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'es2020',
    module: 'commonjs',
    moduleResolution: 'node',
    lib: ['es2020'],
    strict: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

const DECLARATIONS = `export declare const OnEvent: <T = string>(message: T) => MethodDecorator;
export declare function OnNothing<T>(message: T): MethodDecorator;
export declare function OnUnknown(message: unknown): MethodDecorator;
export declare function OnNumber<T = number>(message: T): MethodDecorator;
export declare function OnBoth<T = string>(message: T, body: T): MethodDecorator;
`;

let sequence = 0;
const decorate = (exported: string, extra: Record<string, unknown> = {}): LibraryCheck =>
  ({
    claim_id: `n${sequence++}`,
    package: 'fixture-default-name',
    export: exported,
    role: 'socket',
    receiver: 'export',
    claim: { kind: 'op', op: 'receive', member: null, on: 'export', form: 'decorate', name: { arg: 0 }, ...extra },
  }) as unknown as LibraryCheck;

describe('verify_library_claims: a name slot read through its default (T2b)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-default-name-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      'node_modules/fixture-default-name/package.json': JSON.stringify({
        name: 'fixture-default-name',
        version: '1.0.0',
        types: 'index.d.ts',
      }),
      'node_modules/fixture-default-name/index.d.ts': DECLARATIONS,
    };
    for (const [rel, text] of Object.entries(files)) {
      const file = path.join(root, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, text);
    }
    verifier = new LibraryClaimsVerifier(new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') }));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const verdict = (one: LibraryCheck): string => {
    const [result] = verifier.run(root, [one], 60_000).semantics;
    return result.verdict === 'verified' ? 'verified' : `${result.verdict} ${result.reason}`;
  };

  it('verifies a name typed by an unconstrained parameter defaulting to string', () => {
    assert.strictEqual(verdict(decorate('OnEvent')), 'verified');
  });

  it('states nothing for a parameter with no default, an unknown slot and a default that is no string', () => {
    assert.strictEqual(verdict(decorate('OnNothing')), 'unchecked member_untyped');
    assert.strictEqual(verdict(decorate('OnUnknown')), 'unchecked member_untyped');
    assert.strictEqual(verdict(decorate('OnNumber')), 'unchecked member_untyped');
  });

  it('reads no default for a payload slot', () => {
    const payloadAt1 = verdict(decorate('OnBoth', { payload: { arg: 1 } }));
    assert.ok(!payloadAt1.startsWith('failed name_not_string'), payloadAt1);
  });
});
