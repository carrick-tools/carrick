/**
 * carrick-cloud#3534 T1, contract amendment 12 on carrick#2434: a make may
 * carry `of`, one maker receiver id the export lists. The member is judged on
 * that receiver's type; its declared return is an object type (never the
 * receiver's own, never a Promise); a verified verdict lists the member's
 * declarations. A make without `of` is judged as before.
 *
 * Every package below is invented and its declarations are hand-written.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import { VerifyLibraryClaimsRequestSchema } from '../src/validators.js';
import type { LibraryCheck } from '../src/types.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'es2020',
    module: 'commonjs',
    moduleResolution: 'node',
    lib: ['es2020', 'dom'],
    strict: true,
    esModuleInterop: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

const HUB = `export interface Channel { publish(data: string): void }
export declare class Hub {
  constructor(url?: string);
  emit(data: string): void;
  channel(name: string): Channel;
  on(event: string): this;
  later(name: string): Promise<Channel>;
  count(name: string): number;
}
`;

let sequence = 0;
const check = (receiver: string, claim: Record<string, unknown>): LibraryCheck =>
  ({ claim_id: `m${sequence++}`, package: 'fixture-hub', export: 'Hub', role: 'broker', receiver, claim }) as unknown as LibraryCheck;

const exportMake = (): LibraryCheck => check('export', { kind: 'make', form: 'new', member: null, picker: 'p' });
const ofMake = (extra: Record<string, unknown> = {}, member: string | null = 'channel', receiver = 'instance:new'): LibraryCheck =>
  check(receiver, { kind: 'make', form: 'call', member, of: 'instance:new', name: { arg: 0 }, picker: 'p', ...extra });

describe('verify_library_claims: an of-make (carrick-cloud#3534 T1)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-of-make-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      'node_modules/fixture-hub/package.json': JSON.stringify({ name: 'fixture-hub', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/fixture-hub/index.d.ts': HUB,
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

  const run = (checks: LibraryCheck[]) => verifier.run(root, checks, 60_000).semantics;
  const verdictOf = (one: LibraryCheck): string => {
    const [, result] = run([exportMake(), one]);
    return result.verdict === 'verified' ? 'verified' : `${result.verdict} ${result.reason}`;
  };

  it('verifies on instance:new and lists the member declaration', () => {
    const [exported, made] = run([exportMake(), ofMake()]);
    assert.strictEqual(exported.verdict, 'verified');
    assert.strictEqual(made.verdict, 'verified');
    const line = HUB.split('\n').findIndex(l => l.includes('channel(name')) + 1;
    assert.deepStrictEqual(made.declarations, [{ decl_file: 'node_modules/fixture-hub/index.d.ts', decl_line: line }]);
  });

  it('judges a make without of as before: verified, no declarations', () => {
    const [exported] = run([exportMake()]);
    assert.strictEqual(exported.verdict, 'verified');
    assert.strictEqual(exported.declarations, undefined);
  });

  it('refuses a receiver id that is not a maker (rule 1)', () => {
    for (const of of ['export', 'instance:new>scope:channel', 'type', 'type:Channel', 'instance:']) {
      assert.strictEqual(verdictOf(ofMake({ of })), 'unchecked claim_invalid', of);
    }
  });

  it('wants a call on a named member (rule 2)', () => {
    assert.strictEqual(verdictOf(ofMake({}, null)), 'unchecked claim_invalid');
    assert.strictEqual(verdictOf(ofMake({ form: 'new' })), 'unchecked claim_invalid');
  });

  it('wants a name at an argument, and refuses slots beside it (rule 3)', () => {
    assert.strictEqual(verdictOf(ofMake({ name: undefined })), 'unchecked claim_invalid');
    assert.strictEqual(verdictOf(ofMake({ name: { bound: 'maker' } })), 'unchecked claim_invalid');
    for (const extra of [
      { base: { arg: 0 } },
      { prefix: { arg: 0 } },
      { handler: { arg: 0 } },
      { runs: { arg: 0 } },
      { protocol: 'message' },
      { result: { whole: true } },
    ]) {
      assert.strictEqual(verdictOf(ofMake(extra)), 'unchecked claim_invalid', JSON.stringify(extra));
    }
  });

  it('refuses a fluent return, a Promise and a non-object (rule 4)', () => {
    assert.strictEqual(verdictOf(ofMake({}, 'on')), 'unchecked claim_invalid');
    assert.strictEqual(verdictOf(ofMake({}, 'later')), 'unchecked claim_invalid');
    assert.strictEqual(verdictOf(ofMake({}, 'count')), 'unchecked claim_invalid');
  });

  it('is read on its own receiver only', () => {
    assert.strictEqual(verdictOf(ofMake({}, 'channel', 'export')), 'unchecked receiver_invalid');
  });

  it('opens no receiver id for the made object (rule 7)', () => {
    // The export make names nothing; the of-make's name does not bind `{ bound: 'maker' }` on instance:new.
    const op = check('instance:new', { kind: 'op', op: 'send', member: 'emit', of: 'instance:new', name: { bound: 'maker' }, payload: { arg: 0 } });
    const [, , result] = run([exportMake(), ofMake(), op]);
    assert.strictEqual(`${result.verdict} ${result.reason}`, 'failed name_unbound');
  });

  it('parses in a request, and the schema stays strict elsewhere', () => {
    const request = (claim: Record<string, unknown>) =>
      VerifyLibraryClaimsRequestSchema.safeParse({
        action: 'verify_library_claims',
        from_dir: '/x',
        checks: [{ claim_id: 'a', package: 'fixture-hub', export: 'Hub', role: 'broker', receiver: 'instance:new', claim }],
      });
    assert.ok(request({ kind: 'make', form: 'call', member: 'channel', of: 'instance:new', name: { arg: 0 } }).success);
    assert.ok(!request({ kind: 'make', form: 'call', member: 'channel', via: 'instance:new' }).success);
  });
});
