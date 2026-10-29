/**
 * carrick#2977 ticket B: the verifier reads a claim placed `of: "type"` on a
 * type export, the receiver `type` the surface lister lists, built at its
 * declared type-parameter defaults (`declare const v: Reply;`). A route
 * handler's reply is often a type-only export, so its `send` can now be a
 * label. `yieldsOutcome` is unchanged (decision 11): its tests here pin what
 * it already verifies.
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
import type { LibraryCheck } from '../src/types.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'es2020',
    module: 'commonjs',
    moduleResolution: 'node',
    strict: true,
    esModuleInterop: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

// A route package: the handler is handed a request and a reply that are type
// exports only. `RestReply`'s send is a rest over a conditional tuple, read
// at its default `T = unknown`.
const ROUTES = `export interface Request {
  body: unknown;
}
export interface Reply {
  send(body: unknown): void;
  status(code: number): this;
}
export interface RestReply<T = unknown> {
  send(...args: T extends undefined ? [payload?: T] : [payload: T]): RestReply<T>;
}
export interface Gen<T> {
  send(body: T): void;
}
export declare const notAType: { send(body: unknown): void };
export type Handler = (request: Request, reply: Reply) => void;
export interface Router {
  get(path: string, handler: Handler): this;
}
export declare function router(): Router;
`;

// The middleware shape: a context alias inside the namespace of a module exported whole.
const MIDDLEWARE = `declare function App(): App.Application;
declare namespace App {
  interface Base {
    request: { body: unknown };
  }
  type Context<S = {}> = Base & { state: S; send(body: unknown): void };
  interface Application {
    get(path: string, mw: (ctx: App.Context) => unknown): this;
  }
}
export = App;
`;

let sequence = 0;
const check = (pkg: string, exported: string, receiver: string, claim: Record<string, unknown>): LibraryCheck =>
  ({
    claim_id: `t${sequence++}`,
    package: pkg,
    export: exported,
    role: 'server_framework',
    receiver,
    claim,
  }) as unknown as LibraryCheck;

const make = check('fixture-routes', 'router', 'export', { kind: 'make', form: 'call', member: null });
const route = (yields?: Record<string, unknown>) =>
  check('fixture-routes', 'router', 'instance:()', {
    kind: 'op',
    op: 'receive',
    member: 'get',
    of: 'instance:()',
    name: { arg: 0 },
    handler: { arg: 1 },
    method: 'GET',
    ...(yields ? { yields } : {}),
  });
const send = (exported: string, placement: Record<string, unknown>, receiver = 'type') =>
  check('fixture-routes', exported, receiver, { kind: 'op', op: 'send', member: 'send', payload: { arg: 0 }, ...placement });

describe('verify_library_claims: receiver `type` (carrick#2977 B)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-type-receiver-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
    };
    for (const [name, text] of [
      ['fixture-routes', ROUTES],
      ['fixture-middleware', MIDDLEWARE],
    ]) {
      files[`node_modules/${name}/package.json`] = JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' });
      files[`node_modules/${name}/index.d.ts`] = text;
    }
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

  const verdicts = (checks: LibraryCheck[]): string[] =>
    verifier.run(root, checks, 60_000).semantics.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));

  it('labels the send of a reply that is a type export, on receiver `type`', () => {
    const labels = verifier.labelledMembers(root, [make, route({ param: 1, export: 'Reply', side: 'server' }), send('Reply', { of: 'type' })], 60_000);
    assert.deepStrictEqual(labels.outcomes, { verified: 1 });
    const [key] = [...labels.sends.keys()];
    assert.strictEqual(labels.sends.size, 1);
    assert.match(key, /fixture-routes\/index\.d\.ts:\d+$/);
    const text = fs.readFileSync(path.join(root, 'node_modules/fixture-routes/index.d.ts'), 'utf8');
    assert.ok(text.slice(Number(key.split(':').pop())).startsWith('send(body: unknown): void;'), key);
    assert.deepStrictEqual(labels.sends.get(key), { arg: 0 });
  });

  it('reads a rest over a conditional tuple at the type`s defaults', () => {
    const labels = verifier.labelledMembers(root, [send('RestReply', { of: 'type' })], 60_000);
    assert.deepStrictEqual(labels.outcomes, { verified: 1 });
    assert.strictEqual(labels.sends.size, 1);
  });

  it('reads no `on` claim on `type`, and no type from a const', () => {
    assert.deepStrictEqual(verdicts([send('Reply', { on: 'export' })]), ['unchecked receiver_invalid']);
    assert.deepStrictEqual(verdicts([send('Reply', { on: 'both' })]), ['unchecked receiver_invalid']);
    // `of: "type"` on another receiver, and a scope step after `type`, are refused.
    assert.deepStrictEqual(verdicts([send('Reply', { of: 'type' }, 'export')]), ['unchecked receiver_invalid']);
    assert.deepStrictEqual(verdicts([send('Reply', { of: 'type>scope:x' }, 'type>scope:x')]), ['unchecked claim_invalid']);
    const outcomes = (exported: string) =>
      verifier.labelledMembers(root, [send(exported, { of: 'type' })], 60_000).outcomes;
    assert.deepStrictEqual(outcomes('notAType'), { 'unchecked type_unresolved': 1 });
    // A generic with no default is read at its declared type (ruling R2, carrick#3046).
    assert.deepStrictEqual(outcomes('Gen'), { verified: 1 });
  });

  // Characterisation of decision 11: passes before ticket B.
  it('verifies a `yields` naming the handed type export, and fails one naming another', () => {
    assert.deepStrictEqual(
      verdicts([make, route({ param: 1, export: 'Reply', side: 'server' }), route({ param: 1, export: 'Request', side: 'server' })]),
      ['verified', 'verified', 'failed yields_not_export']
    );
    const middleware = [
      check('fixture-middleware', 'default', 'export', { kind: 'make', form: 'call', member: null }),
      check('fixture-middleware', 'default', 'instance:()', {
        kind: 'op',
        op: 'receive',
        member: 'get',
        of: 'instance:()',
        name: { arg: 0 },
        handler: { arg: 1 },
        method: 'GET',
        yields: { param: 0, export: 'Context', side: 'server' },
      }),
    ];
    assert.deepStrictEqual(verdicts(middleware), ['verified', 'verified']);
  });
});
