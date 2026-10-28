/**
 * carrick#2674: reading a service's server labels for an `infer` request
 * leaves the inferrer's program as it was, and a read that fails answers no
 * labels instead of failing the request.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { ProjectLoader } from '../src/project-loader.js';
import { readServerLabels, type LabelSource } from '../src/server-labels.js';

const PKG = 'reply-kit';
const KIT = `export interface Rep<B = unknown> {
  send(payload?: B): Rep<B>;
}
export declare const reply: Rep;
`;
const APP = `import { reply, type Rep } from '${PKG}';
export function handler(rep: Rep<{ id: string }>): void {
  rep.send({ id: 'a' });
}
void reply;
`;
// A route package (carrick#2968): a receive op whose request names a body
// member the request type lacks verifies with its request dropped, so that
// body is never read.
const ROUTES = 'route-kit';
const ROUTE_KIT = `export interface Req {
  path: string;
  body: { id: string };
}
export interface Routes {
  post(path: string, handler: (req: Req) => void): this;
  put(path: string, handler: (req: Req) => void): this;
}
export declare const routes: Routes;
`;
const routeOp = (claim_id: string, member: string, body: string[]) => ({
  claim_id,
  package: ROUTES,
  export: 'routes',
  role: 'server_framework',
  receiver: 'export',
  claim: {
    kind: 'op',
    op: 'receive',
    member,
    on: 'export',
    name: { arg: 0 },
    handler: { arg: 1 },
    method: member.toUpperCase(),
    request: { param: 0, path: ['path'], body },
  },
});
const CLAIMS = {
  from_dir: '.',
  checks: [
    {
      claim_id: 'send',
      package: PKG,
      export: 'reply',
      role: 'server_framework',
      receiver: 'export',
      claim: { kind: 'op', op: 'send', member: 'send', on: 'export', payload: { arg: 0 } },
    },
  ],
};

describe('carrick#2674: server labels for an infer request', () => {
  let root: string;
  let loader: ProjectLoader;
  let source: LabelSource;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-server-labels-'));
    fs.writeFileSync(
      path.join(root, 'tsconfig.json'),
      JSON.stringify({ compilerOptions: { strict: true, module: 'commonjs', types: [] }, include: ['src/**/*.ts'] })
    );
    fs.mkdirSync(path.join(root, 'node_modules', PKG), { recursive: true });
    fs.writeFileSync(
      path.join(root, 'node_modules', PKG, 'package.json'),
      JSON.stringify({ name: PKG, version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(root, 'node_modules', PKG, 'index.d.ts'), KIT);
    fs.mkdirSync(path.join(root, 'node_modules', ROUTES), { recursive: true });
    fs.writeFileSync(
      path.join(root, 'node_modules', ROUTES, 'package.json'),
      JSON.stringify({ name: ROUTES, version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(root, 'node_modules', ROUTES, 'index.d.ts'), ROUTE_KIT);
    fs.mkdirSync(path.join(root, 'src'));
    fs.writeFileSync(path.join(root, 'src', 'app.ts'), APP);
    loader = new ProjectLoader({ repoRoot: root });
    assert.ok(loader.load().success);
    source = {
      getRepoRoot: () => loader.getRepoRoot(),
      scriptProjectFor: (key) => loader.scriptProjectFor(key),
      packageDirectoryOf: (file) => loader.packageDirectoryOf(file),
      resolveModule: (name, from, options) => loader.resolveThroughGraph(name, from, options),
    };
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('reads the labels without rebuilding the program the inferrer reads', () => {
    const project = loader.getProjectFor('');
    const before = project.getProgram().compilerObject;
    const checker = before.getTypeChecker();
    const labels = readServerLabels(source, '', CLAIMS, 60_000, () => {});
    assert.strictEqual(labels.sends.size, 1);
    const after = project.getProgram().compilerObject;
    assert.strictEqual(after, before, 'the inferrer’s program was rebuilt');
    assert.strictEqual(after.getTypeChecker(), checker);
  });

  it('a read that throws answers no labels and says why', () => {
    const logged: string[] = [];
    const failing: LabelSource = {
      ...source,
      scriptProjectFor: () => {
        throw new Error('no program');
      },
    };
    const labels = readServerLabels(failing, '', CLAIMS, 60_000, (message) => logged.push(message));
    assert.strictEqual(labels.sends.size, 0);
    assert.strictEqual(labels.bodies.size, 0);
    assert.ok(logged.some((line) => line.includes('no program')));
  });

  it('a check the schema refuses is dropped and the rest still read', () => {
    const logged: string[] = [];
    const refused = { ...CLAIMS.checks[0], claim_id: 'bad', claim: { kind: 'op', op: 'teleport' } };
    const labels = readServerLabels(
      source,
      '',
      { ...CLAIMS, checks: [refused, ...CLAIMS.checks] },
      60_000,
      (message) => logged.push(message)
    );
    assert.strictEqual(labels.sends.size, 1);
    assert.ok(logged.some((line) => line.startsWith('Dropped 1 server claim')));
  });

  it('a request member the declarations lack drops that request, never the route, and is counted (carrick#2968)', () => {
    const logged: string[] = [];
    const labels = readServerLabels(
      source,
      '',
      { from_dir: '.', checks: [routeOp('kept', 'post', ['body']), routeOp('dropped', 'put', ['payload'])] },
      60_000,
      (message) => logged.push(message)
    );
    assert.strictEqual(labels.bodies.size, 1, 'only the request that resolves names a body');
    assert.deepStrictEqual(labels.outcomes, {
      verified: 1,
      'verified request_dropped request_member_missing': 1,
    });
    assert.ok(logged.some((line) => line.includes('request_dropped request_member_missing')));
  });
});
