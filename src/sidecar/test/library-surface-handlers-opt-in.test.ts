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

// Four route methods handing one ten-member request type: a listing that
// inlines the type per method is four times the type's size.
const REQ = Array.from({ length: 10 }, (_, i) => `  r${i}: string;`).join('\n');
const ROUTES = `export interface Req {\n${REQ}\n}
export declare class Router {
  del(path: string, handler: (req: Req) => void): this;
  get(path: string, handler: (req: Req) => void): this;
  post(path: string, handler: (req: Req) => void): this;
  put(path: string, handler: (req: Req) => void): this;
}
`;

interface Member {
  name: string;
  handlers?: Array<{ arg: number; params: Array<{ name: string; type_id: number }> }>;
}
interface Entry {
  id: number;
  type: string;
  dropped: number;
  members: Array<{ path: string[] }>;
}
interface Listed {
  status: string;
  errors?: string[];
  surface_sha256: string;
  surfaces: Array<{
    handler_types?: Entry[];
    handler_params_left_out?: number;
    exports: Array<{ receivers: Array<{ members: Member[] }> }>;
  }>;
}

/** The `handler_types` entry a parameter refers to (carrick#2977). */
const entryOf = (listed: Listed, typeId: number): Entry => {
  const entry = listed.surfaces[0].handler_types?.find(each => each.id === typeId);
  assert.ok(entry, `type_id ${typeId} has no entry`);
  return entry;
};

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
      'node_modules/fixture-routes/package.json': JSON.stringify({ name: 'fixture-routes', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/fixture-routes/index.d.ts': ROUTES,
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
      assert.deepStrictEqual(entryOf(listed, handlers[0].params[0].type_id).members.map(m => m.path.join('.')), ['ack', 'payload', 'topic']);
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
      const paths = entryOf(listed, byName.get(name)!.handlers![0].params[0].type_id).members.map(m => m.path.join('.'));
      // Each method keeps a cap of its own, not what the other left.
      assert.strictEqual(paths.length, 500, name);
      // `zz` sorts after 600 nested members, and is kept: top-level first.
      assert.deepStrictEqual(paths.slice(0, 3), ['head', 'nested', 'nested.n000']);
      assert.strictEqual(paths[paths.length - 1], 'zz');
      assert.strictEqual(paths[paths.length - 2], 'nested.n496');
    }
    // 603 members per type (three top-level, 600 nested), 500 kept: each
    // type's entry counts its own drop once (carrick#2977 decision 8).
    assert.deepStrictEqual(listed.surfaces[0].handler_types!.map(e => [e.type, e.dropped]), [['WideA', 103], ['WideB', 103]]);
    const without = await list({}, 'fixture-server');
    assert.strictEqual(without.surfaces[0].handler_types, undefined);
    assert.strictEqual(without.surface_sha256, listed.surface_sha256);
  });

  // carrick#3016: the response is one JSON line, and a string passes V8's
  // limit near 512 MB. A server framework handed one request type by thousands
  // of members listed over a gigabyte of members, 50 distinct types inlined
  // per member. A listing spends one budget of handler members across the
  // whole call. With the type table (carrick#2977) a type's members are
  // written once, so only a new entry spends it: a parameter whose new entry
  // would pass it is left out and counted, and a reference costs nothing.
  it('spends one budget of handler members per listing on new entries, counts what it leaves out and keeps the hash', async () => {
    const free = await list({ handlers: true }, 'fixture-routes');
    const all = membersOf(free).filter(m => m.handlers !== undefined).map(m => m.name);
    assert.deepStrictEqual(all, ['del', 'get', 'post', 'put']);
    assert.strictEqual(free.surfaces[0].handler_params_left_out, 0);
    assert.strictEqual(free.surfaces[0].handler_types!.length, 1);

    // 10 members: the one ten-member entry fits, so every method refers to it.
    const fits = await list({ handlers: true, max_handler_members: 10 }, 'fixture-routes');
    assert.deepStrictEqual(membersOf(fits).filter(m => m.handlers !== undefined).map(m => m.name), all);
    assert.strictEqual(fits.surfaces[0].handler_params_left_out, 0);

    // 9 members: the entry would pass it, so all four parameters are left out.
    const bounded = await list({ handlers: true, max_handler_members: 9 }, 'fixture-routes');
    assert.deepStrictEqual(membersOf(bounded).filter(m => m.handlers !== undefined).map(m => m.name), []);
    assert.deepStrictEqual(bounded.surfaces[0].handler_types, []);
    assert.strictEqual(bounded.surfaces[0].handler_params_left_out, 4);
    assert.strictEqual(bounded.surface_sha256, free.surface_sha256);
    const without = await list({}, 'fixture-routes');
    assert.strictEqual(without.surface_sha256, bounded.surface_sha256);
  });
});
