/**
 * carrick#3054: the reply a route handler returns, read through the `returns`
 * field of the route's receive op (reply-by-return).
 *
 * Two invented server packages:
 * - `fixture-routes`: `get<R>(path, handler: (req) => R | Promise<R>)`, where
 *   the route's type parameter is whatever the handler returns, plus a
 *   labelled `reply.send` member for a handler that sends instead;
 * - `fixture-typed-routes`: `get<P, Out>(path, h: (c) => Reply<Out>)`, whose
 *   label names the `body` member of `Reply` as the response.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

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

const ROUTES = `export interface Req { params: Record<string, string> }
export interface Rep { send(payload: { ok: boolean }): void }
export interface App {
  get<R>(path: string, handler: (req: Req, rep: Rep) => R | Promise<R>): App;
}
declare function make(): App;
export default make;
export declare const reply: Rep;
`;

const TYPED = `export interface Reply<T> { body: T; status: number }
export interface Ctx<P extends string> { path: P }
export interface App {
  get<P extends string, Out>(path: P, h: (c: Ctx<P>) => Reply<Out> | Promise<Reply<Out>>): App;
}
declare function make(): App;
export default make;
export declare function ok<T>(body: T): Reply<T>;
`;

const APP = `import make from 'fixture-routes';
import { reply } from 'fixture-routes';
import typed, { ok } from 'fixture-typed-routes';

interface User { id: number; name: string }
declare const found: User | undefined;
declare function lookup(): Promise<User>;
declare function loose(): any;

const app = make();
const t = typed();

// A
app.get('/u', async () => ({ id: 1, name: 'a' }));
// B
app.get('/b', async () => { if (!found) return undefined; return found; });
// C
t.get('/t', async () => ok({ id: 1, name: 'a' }));
// D
app.get('/d', async () => loose());
// E
app.get('/e', async (req, rep) => { rep.send({ ok: true }); });
// F
app.get('/f', async (req, rep) => { rep.send({ ok: true }); return lookup(); });
`;

let sequence = 0;
type Claim = Record<string, unknown> & { kind: string };
const check = (pkg: string, exported: string, receiver: string, claim: Claim) => ({
  claim_id: `r${sequence++}`,
  package: pkg,
  export: exported,
  role: 'server_framework',
  receiver,
  claim,
});
const make = (pkg: string) => check(pkg, 'default', 'export', { kind: 'make', form: 'call', member: null });
const receive = (pkg: string, returns?: Record<string, unknown>) =>
  check(pkg, 'default', 'instance:()', {
    kind: 'op',
    op: 'receive',
    member: 'get',
    of: 'instance:()',
    name: { arg: 0 },
    handler: { arg: 1 },
    method: 'GET',
    ...(returns ? { returns } : {}),
  });
const send = check('fixture-routes', 'reply', 'export', {
  kind: 'op',
  op: 'send',
  member: 'send',
  on: 'export',
  payload: { arg: 0 },
});

const LABELS = [
  make('fixture-routes'),
  receive('fixture-routes', {}),
  send,
  make('fixture-typed-routes'),
  receive('fixture-typed-routes', { result: { member: ['body'] } }),
];
const WITHOUT_RETURNS = [
  make('fixture-routes'),
  receive('fixture-routes'),
  send,
  make('fixture-typed-routes'),
  receive('fixture-typed-routes'),
];

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ path: string; reason: string }>;
}

describe('carrick#3054: the reply a handler returns', () => {
  let root: string;
  let appPath: string;
  let client: SidecarClient;
  let id = 0;

  const install = (name: string, source: string) => {
    fs.mkdirSync(path.join(root, 'node_modules', name), { recursive: true });
    fs.writeFileSync(
      path.join(root, 'node_modules', name, 'package.json'),
      JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(root, 'node_modules', name, 'index.d.ts'), source);
  };

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-stated-returns-'));
    fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
    install('fixture-routes', ROUTES);
    install('fixture-typed-routes', TYPED);
    fs.mkdirSync(path.join(root, 'src'));
    appPath = path.join(root, 'src', 'app.ts');
    fs.writeFileSync(appPath, APP);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: root });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  const lineOf = (label: string): number => APP.split('\n').indexOf(`// ${label}`) + 2;

  const reply = async (label: string, checks: unknown[] = LABELS): Promise<Inferred | undefined> => {
    const alias = `${label}_${id}`;
    const response = await client.send<{ inferred_types?: Inferred[] }>(
      {
        action: 'infer',
        request_id: `infer-${id++}`,
        requests: [{ file_path: appPath, line_number: lineOf(label), infer_kind: 'response_body', alias }],
        server_claims: { from_dir: root, checks },
      },
      60_000
    );
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  const normalised = (text: string | undefined) => (text ?? '').replace(/\s+/g, ' ');

  it('A: the value an async handler returns is the reply', async () => {
    const got = await reply('A');
    assert.match(normalised(got?.type_string), /id: number/);
    assert.match(normalised(got?.type_string), /name: string/);
  });

  it('B: a handler that may return undefined replies with the other part only', async () => {
    const got = await reply('B');
    assert.match(normalised(got?.type_string), /id: number/);
    assert.doesNotMatch(normalised(got?.type_string), /undefined/);
  });

  it('C: the label picks the body member of the route-typed return', async () => {
    const got = await reply('C');
    assert.match(normalised(got?.type_string), /id: number/);
    assert.doesNotMatch(normalised(got?.type_string), /status/);
  });

  it('D: a handler returning any states no reply', async () => {
    const got = await reply('D');
    assert.doesNotMatch(normalised(got?.type_string), /id: number/);
  });

  it('E and F: a handler that calls a labelled send, with or without a return, reads as it does without the returns label', async () => {
    for (const label of ['E', 'F']) {
      const withReturns = await reply(label);
      const without = await reply(label, WITHOUT_RETURNS);
      assert.deepStrictEqual(
        { ...withReturns, alias: '' },
        { ...without, alias: '' },
        label
      );
    }
  });

  it('without the returns label, B keeps its undefined part and C keeps the Reply envelope', async () => {
    assert.match(normalised((await reply('B', WITHOUT_RETURNS))?.type_string), /undefined/);
    assert.match(normalised((await reply('C', WITHOUT_RETURNS))?.type_string), /Reply</);
  });
});
