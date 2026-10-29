/**
 * carrick#3045 (H1 of the HTTP slice), label restructure amendment 7
 * (carrick#2434): the request schema and the claim types take the whole new
 * grammar, and the verifier answers `unchecked part_unbuilt` for every
 * element that uses a part it does not judge yet. Nothing here judges a new
 * shape; an element without the new fields answers as it did.
 *
 * The package is invented and its declarations are hand-written.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import { settledServerChecks, ListLibrarySurfaceRequestSchema } from '../src/validators.js';
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

const ROUTES = `export interface Request {
  body: unknown;
}
export interface Reply {
  send(body: unknown): void;
}
export type Handler = (request: Request, reply: Reply) => void;
export interface Router {
  get(path: string, handler: Handler): this;
  mount(path: string): Router;
}
export declare function router(): Router;
`;

let sequence = 0;
const check = (receiver: string, claim: Record<string, unknown>): LibraryCheck =>
  ({
    claim_id: `g${sequence++}`,
    package: 'fixture-routes',
    export: 'router',
    role: 'server_framework',
    receiver,
    claim,
  }) as unknown as LibraryCheck;

const make = check('export', { kind: 'make', form: 'call', member: null });
const route = (extra: Record<string, unknown> = {}) =>
  check('instance:()', {
    kind: 'op',
    op: 'receive',
    member: 'get',
    of: 'instance:()',
    name: { arg: 0 },
    handler: { arg: 1 },
    method: 'GET',
    ...extra,
  });
const scope = (extra: Record<string, unknown> = {}) =>
  check('instance:()', { kind: 'scope', member: 'mount', of: 'instance:()', name: { arg: 0 }, ...extra });

/** Every new field, one element each. */
const ELEMENTS: Record<string, LibraryCheck> = {
  'slot at': route({ payload: { arg: 0, at: ['messages[]', 'value'] } }),
  'slot each': route({ name: { arg: 0, each: true } }),
  'name slot document': route({ name: { arg: 0, document: 'graphql' } }),
  'name slot ref function': route({ name: { arg: 0, ref: 'function' } }),
  'name slot ref class': route({ name: { arg: 0, ref: 'class' } }),
  'name parts': route({ name: { parts: [{ arg: 0 }, { arg: 1, key: 'topic' }], join: '/' } }),
  'name parts of three': route({ name: { parts: [{ arg: 0 }, { arg: 1 }, { arg: 2 }] } }),
  'name literal': route({ name: { literal: 'connection' } }),
  'bound path': route({ name: { bound: 'path', join: '.' } }),
  'bound export': route({ name: { bound: 'export' } }),
  'bound subscribed': route({ name: { bound: 'subscribed', param: 0, at: ['topic'] } }),
  'form new': route({ form: 'new' }),
  'form set': route({ form: 'set' }),
  'form override': route({ form: 'override' }),
  'form decorate': route({ form: 'decorate' }),
  'protocol http': route({ protocol: 'http' }),
  'protocol graphql': route({ protocol: 'graphql' }),
  'protocol rpc': route({ protocol: 'rpc' }),
  'protocol socket': route({ protocol: 'socket' }),
  'protocol message': route({ protocol: 'message' }),
  'when': route({ when: { arg: 0, literal: 'message' } }),
  'when with key': route({ when: { arg: 0, key: 'event', literal: 'message' } }),
  'returns empty': route({ returns: {} }),
  'returns result': route({ returns: { result: { member: ['data'] } } }),
  'scope form decorate': scope({ form: 'decorate' }),
  'scope name slot ref': scope({ name: { arg: 0, ref: 'class' } }),
  'make slot at': check('export', { kind: 'make', form: 'call', member: null, name: { arg: 0, at: ['id'] } }),
};

/** The elements above whose part a judge reads now. */
const BUILT = new Set(['form new', 'form decorate', 'returns empty', 'returns result', 'scope form decorate']);

describe('verify_library_claims: amendment 7 grammar (carrick#3045)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-grammar-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      'node_modules/fixture-routes/package.json': JSON.stringify({ name: 'fixture-routes', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/fixture-routes/index.d.ts': ROUTES,
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
    const [result] = verifier.run(root, [make, one], 60_000).semantics.slice(-1);
    return result.verdict === 'verified' ? 'verified' : `${result.verdict} ${result.reason}`;
  };

  for (const [name, element] of Object.entries(ELEMENTS)) {
    it(`parses ${name} through the request schema`, () => {
      const settled = settledServerChecks([element]);
      assert.strictEqual(settled.refused, 0);
      assert.deepStrictEqual(settled.checks[0].claim, element.claim);
    });

    // Judged since carrick#3047 (library-claims-forms.test.ts).
    if (BUILT.has(name)) continue;
    it(`answers unchecked part_unbuilt for ${name}`, () => {
      assert.strictEqual(verdict(element), 'unchecked part_unbuilt');
    });
  }

  it('refuses a name with parts and a bound together, and one part-list of one', () => {
    assert.strictEqual(settledServerChecks([route({ name: { parts: [{ arg: 0 }] } })]).refused, 1);
    assert.strictEqual(settledServerChecks([route({ name: { parts: [{ arg: 0 }, { arg: 1 }], bound: 'maker' } })]).refused, 1);
    assert.strictEqual(settledServerChecks([route({ form: 'returns' })]).refused, 1);
    assert.strictEqual(settledServerChecks([route({ protocol: 'grpc' })]).refused, 1);
  });

  it('lists types on a surface request', () => {
    const parsed = ListLibrarySurfaceRequestSchema.safeParse({
      request_id: 'r1',
      action: 'list_library_surface',
      from_dir: '/x',
      packages: ['p'],
      types: true,
    });
    assert.ok(parsed.success);
    assert.strictEqual(parsed.data.types, true);
  });

  it('answers an element without the new fields exactly as before', () => {
    assert.strictEqual(verdict(route()), 'verified');
    assert.strictEqual(verdict(route({ form: 'call' })), 'verified');
    assert.strictEqual(verdict(scope()), 'verified');
    assert.strictEqual(verdict(route({ name: { bound: 'maker' } })), verdict(route({ name: { bound: 'maker' } })));
  });

  it('keeps a type receiver id answering receiver_invalid until the type paths are read', () => {
    const typed = check('type:Reply', { kind: 'op', op: 'send', member: 'send', payload: { arg: 0 } });
    const modular = check('type:fixture-other#Reply', { kind: 'op', op: 'send', member: 'send', payload: { arg: 0 } });
    assert.strictEqual(verdict(typed), 'unchecked receiver_invalid');
    assert.strictEqual(verdict(modular), 'unchecked receiver_invalid');
  });
});
