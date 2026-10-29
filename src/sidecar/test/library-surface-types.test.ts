/**
 * carrick#3050 (H6): `list_library_surface` with `types: true` lists `types[]`
 * for every exported class and every type a listed maker or member returns or
 * a handler is handed, with the receiver id of amendment 7, and `settable[]`
 * on each. All of it sits outside `surface_sha256` and `truncated`.
 *
 * Every package below is invented and its declarations are hand-written.
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

// A types package: a router type with a base, and the scope a route makes.
const CORE = `export interface Base {
  parent(): this;
}
export interface Route {
  handle(path: string, callback: () => void): this;
}
export interface Router extends Base {
  route(path: string): Route;
  base(): Base;
  listen(port: number): void;
}
`;

// A server package that returns the types package's Router and exports a class.
const SERVER = `import type { Router } from 'fixture-core';
export declare function server(): Router;
export declare class Server {
  listen(port: number): void;
}
`;

// A module exported whole, whose namespace declares the context every handler is handed.
const APP = `declare function App(): App.Context;
declare namespace App {
  interface Context {
    body: unknown;
    status: number;
    readonly ip: string;
    json(body: unknown): void;
  }
}
export = App;
`;

// A router whose route handlers are handed fixture-app's Context.
const ROUTER = `import App = require('fixture-app');
export interface Router {
  get(path: string, handler: (ctx: App.Context) => void): this;
}
export declare function router(): Router;
`;

const CLASSES = `export declare class Alpha { a(): void; }
export declare class Beta { b(): void; }
export declare class Gamma { c(): void; }
`;

interface Settable {
  name: string;
  type: string;
  own: boolean;
}
interface TypeReceiver {
  receiver: string;
  members: Array<{ name: string }>;
  settable?: Settable[];
}
interface Surface {
  package: string;
  truncated: number;
  exports: unknown[];
  handler_types?: Array<{ id: number; export?: string }>;
  types?: Array<{ export: string; receivers: TypeReceiver[] }>;
  types_left_out?: number;
}

describe('list_library_surface: types (carrick#3050)', () => {
  let client: SidecarClient;
  let root: string;
  let requestId = 0;

  async function surface(packages: string[], extra: Record<string, unknown> = {}): Promise<Surface[]> {
    const response = await client.send<{ status: string; surfaces: Surface[]; surface_sha256: string; errors?: string[] }>(
      { request_id: `t-${requestId++}`, action: 'list_library_surface', from_dir: root, packages, ...extra },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response.surfaces;
  }

  const ids = (listed: Surface): Array<[string, string]> =>
    (listed.types ?? []).flatMap(entry => entry.receivers.map(r => [r.receiver, entry.export] as [string, string]));

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-types-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
    };
    for (const [name, text] of [
      ['fixture-core', CORE],
      ['fixture-server', SERVER],
      ['fixture-app', APP],
      ['fixture-router', ROUTER],
      ['fixture-classes', CLASSES],
    ]) {
      files[`node_modules/${name}/package.json`] = JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' });
      files[`node_modules/${name}/index.d.ts`] = text;
    }
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

  it('lists an exported class as `type` and what a maker or member returns by its declaring module', async () => {
    const [listed] = await surface(['fixture-server'], { types: true });
    assert.deepStrictEqual(ids(listed), [
      ['type', 'Server'],
      ['type:fixture-core#Base', 'server'],
      ['type:fixture-core#Route', 'server'],
      ['type:fixture-core#Router', 'server'],
    ]);
    const router = listed.types!.find(entry => entry.receivers[0].receiver === 'type:fixture-core#Router')!;
    assert.deepStrictEqual(
      router.receivers[0].members.map(m => m.name),
      ['base', 'listen', 'parent', 'route']
    );
  });

  it('lists nothing under `types` without the key', async () => {
    const [listed] = await surface(['fixture-server'], { handlers: true });
    // #3025's listing: only the type exports a handler names; no budget count.
    assert.deepStrictEqual(listed.types, []);
    assert.strictEqual(listed.types_left_out, undefined);
  });

  it('names a handed type declared in another package, and lists its settable members', async () => {
    const [listed] = await surface(['fixture-router'], { types: true, handlers: true });
    assert.deepStrictEqual(listed.handler_types!.map(entry => entry.export), ['type:fixture-app#Context']);
    assert.deepStrictEqual(ids(listed), [
      ['type', 'Router'],
      ['type:fixture-app#Context', 'router'],
    ]);
    const context = listed.types!.find(entry => entry.receivers[0].receiver === 'type:fixture-app#Context')!.receivers[0];
    assert.deepStrictEqual(context.settable, [{ name: 'body', type: 'unknown', own: true }]);
    // Without `types`, the entry names nothing: the listing is the one #3025 gave.
    const [plain] = await surface(['fixture-router'], { handlers: true });
    assert.deepStrictEqual(plain.handler_types!.map(entry => entry.export), [undefined]);
    assert.strictEqual(plain.types?.length ?? 0, 0);
  });

  it('leaves the hash, `truncated` and the exports as they are', async () => {
    for (const pkg of ['fixture-server', 'fixture-router']) {
      const hash = async (extra: Record<string, unknown>) => {
        const response = await client.send<{ surface_sha256: string; surfaces: Surface[] }>(
          { request_id: `h-${requestId++}`, action: 'list_library_surface', from_dir: root, packages: [pkg], ...extra },
          60_000
        );
        return response;
      };
      const without = await hash({ handlers: true });
      const withTypes = await hash({ handlers: true, types: true });
      assert.strictEqual(withTypes.surface_sha256, without.surface_sha256);
      assert.strictEqual(withTypes.surfaces[0].truncated, without.surfaces[0].truncated);
      assert.deepStrictEqual(withTypes.surfaces[0].exports, without.surfaces[0].exports);
    }
  });

  it('spends a budget of its own and counts the entries it leaves out', async () => {
    // Three exported classes are three entries however little of the main listing fits.
    const [listed] = await surface(['fixture-classes'], { types: true, max_entries: 1 });
    assert.deepStrictEqual(ids(listed), [['type', 'Alpha']]);
    assert.strictEqual(listed.types_left_out, 2);
    const [roomy] = await surface(['fixture-classes'], { types: true });
    assert.deepStrictEqual(ids(roomy), [['type', 'Alpha'], ['type', 'Beta'], ['type', 'Gamma']]);
    assert.strictEqual(roomy.types_left_out, 0);
  });
});
