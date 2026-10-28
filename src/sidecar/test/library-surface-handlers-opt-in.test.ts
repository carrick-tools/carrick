/**
 * carrick#2966: `list_library_surface` lists handler-parameter members
 * (carrick#2674 S3) only when the request sets `handlers: true`. Without it the
 * handler pass does not run, and the hash is the same either way.
 *
 * The package below is invented and its declarations are hand-written.
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

// Two handler parameters, each wider than the cap: a top-level `head`, a
// top-level `nested` holding 600 members, and a top-level `zz` that sorts last,
// 603rd in parent-before-child order. Each type is its own, so neither shares
// the other's listing.
const NESTED = Array.from({ length: 600 }, (_, i) => `    n${String(i).padStart(3, '0')}: string;`).join('\n');
const WIDE = ['WideA', 'WideB']
  .map(name => `export interface ${name} {\n  head: string;\n  nested: {\n${NESTED}\n  };\n  zz: number;\n}`)
  .join('\n');
const SERVER = `${WIDE}
export declare class Server {
  first(handler: (req: WideA) => void): this;
  second(handler: (req: WideB) => void): this;
}
`;

const BUS = `export interface Ctx {
  topic: string;
  payload: unknown;
  ack(): void;
}
export declare class Bus {
  subscribe(topic: string, handler: (ctx: Ctx) => void): this;
  use(handler: (ctx: Ctx) => void): this;
}
`;

interface Member {
  name: string;
  handlers?: Array<{ arg: number; params: Array<{ name: string; members: Array<{ path: string[] }> }> }>;
}
interface Listed {
  status: string;
  errors?: string[];
  surface_sha256: string;
  surfaces: Array<{ handlers_truncated?: number; exports: Array<{ receivers: Array<{ members: Member[] }> }> }>;
}

describe('list_library_surface: handler members only on request (carrick#2966)', () => {
  let client: SidecarClient;
  let root: string;
  let requestId = 0;

  async function list(extra: Record<string, unknown> = {}, pkg = 'fixture-bus'): Promise<Listed> {
    const response = await client.send<Listed>(
      { request_id: `h-${requestId++}`, action: 'list_library_surface', from_dir: root, packages: [pkg], ...extra },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response;
  }

  const membersOf = (listed: Listed): Member[] =>
    listed.surfaces.flatMap(s => s.exports.flatMap(e => e.receivers.flatMap(r => r.members)));

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-handlers-opt-in-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      'node_modules/fixture-bus/package.json': JSON.stringify({ name: 'fixture-bus', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/fixture-bus/index.d.ts': BUS,
      'node_modules/fixture-server/package.json': JSON.stringify({ name: 'fixture-server', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/fixture-server/index.d.ts': SERVER,
    };
    for (const [rel, text] of Object.entries(files)) {
      const file = path.join(root, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, text);
    }
    client = new SidecarClient();
    await client.start();
    const init = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('lists no handlers when the request does not ask for them', async () => {
    const listed = await list();
    assert.deepStrictEqual(membersOf(listed).map(m => m.name).sort(), ['subscribe', 'use']);
    assert.ok(!JSON.stringify(listed.surfaces).includes('"handlers'), 'a member carries handlers');
  });

  it('lists them for both methods with handlers: true', async () => {
    const listed = await list({ handlers: true });
    const byName = new Map(membersOf(listed).map(m => [m.name, m]));
    for (const [name, arg] of [['subscribe', 1], ['use', 0]] as const) {
      const handlers = byName.get(name)!.handlers;
      assert.ok(handlers, `${name} lists no handlers`);
      assert.deepStrictEqual(handlers.map(h => h.arg), [arg]);
      assert.deepStrictEqual(handlers[0].params[0].members.map(m => m.path.join('.')), ['ack', 'payload', 'topic']);
    }
  });

  it('hashes the surface the same with and without handlers', async () => {
    const without = await list();
    const withHandlers = await list({ handlers: true });
    assert.strictEqual(without.surface_sha256, withHandlers.surface_sha256);
  });

  it('caps each handler parameter at 500 members, top-level first, and counts what it drops', async () => {
    const listed = await list({ handlers: true }, 'fixture-server');
    const byName = new Map(membersOf(listed).map(m => [m.name, m]));
    for (const name of ['first', 'second']) {
      const paths = byName.get(name)!.handlers![0].params[0].members.map(m => m.path.join('.'));
      // Each method keeps a cap of its own, not what the other left.
      assert.strictEqual(paths.length, 500, name);
      // `zz` sorts after 600 nested members, and is kept: top-level first.
      assert.deepStrictEqual(paths.slice(0, 3), ['head', 'nested', 'nested.n000']);
      assert.strictEqual(paths[paths.length - 1], 'zz');
      assert.strictEqual(paths[paths.length - 2], 'nested.n496');
    }
    // 603 members per parameter (three top-level, 600 nested), 500 kept, for two parameters.
    assert.strictEqual(listed.surfaces[0].handlers_truncated, 206);
    const without = await list({}, 'fixture-server');
    assert.strictEqual(without.surfaces[0].handlers_truncated, undefined);
    assert.strictEqual(without.surface_sha256, listed.surface_sha256);
  });
});
