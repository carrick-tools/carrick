/**
 * carrick#2774 T1: a member of an instance receiver whose call returns another
 * object that routes are registered on lists that object as `scope_receiver`
 * (receiver id `R>scope:m`). It has a cap of its own and is left out of
 * `surface_sha256`, so a listing without it, `truncated` and the hash are the
 * ones the lister gave before.
 *
 * Every package below is invented and its declarations are hand-written.
 * `fixtures/library-surface-scope-receivers.golden.json` holds what the lister
 * of the commit before this change gave for the same package: the hash, and
 * the listing under a tiny `max_entries`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { fullSurfaceSha256 } from '../src/library-claims.js';
import type { LibrarySurface } from '../src/types.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GOLDEN = path.join(here, '..', '..', 'test', 'fixtures', 'library-surface-scope-receivers.golden.json');

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

const FOREIGN = `export interface Foreign {
  get(...handlers: Array<() => void>): this;
}
`;

const ROUTER = `import { Foreign } from 'fixture-foreign';
export type Handler = (request: { body: unknown }, reply: { send(body: unknown): void }) => void;
export interface Route {
  get(...handlers: Handler[]): this;
  post(...handlers: Handler[]): this;
  delete(...handlers: Handler[]): this;
}
export interface Route2<P extends string = string> {
  get(...handlers: Handler[]): this;
  readonly path: P;
}
export interface Pending<T> {
  then(onDone: (value: T) => void): void;
  get(...handlers: Handler[]): this;
}
export interface Other {
  head(...handlers: Handler[]): this;
}
export interface Step {
  run(...handlers: Handler[]): this;
}
export interface Wrap extends Step {
  inner(): void;
}
export interface App extends Step {
  get(path: string, ...handlers: Handler[]): this;
  use(prefix: string, child: App): this;
  at(path: string): Route;
  basePath(path: string): this;
  open(path: string): Pending<Route>;
  wait(path: string): Promise<Route>;
  label(s: string): string;
  either(path: string): Route | Other;
  fn(path: string): () => void;
  count(n: number): Route;
  ext(path: string): Foreign;
  wrap(label: string): Wrap;
  at2<P extends string>(path: P): Route2<P>;
  at2(path: string, options: {}): Route2<string>;
}
export declare function app(): App;
`;

/** The members that carry a scope receiver; every other member of the fixture carries none. */
const LISTED = ['at', 'at2'];
const NOTHING = ['basePath', 'count', 'either', 'ext', 'fn', 'get', 'label', 'open', 'run', 'use', 'wait', 'wrap'];

interface Member {
  name: string;
  scope_receiver?: { receiver: string; members: Member[]; scope_receiver?: unknown };
}
interface Receiver {
  receiver: string;
  members: Member[];
}
interface Listed {
  status: string;
  errors?: string[];
  surface_sha256: string;
  surfaces: Array<LibrarySurface & { exports: Array<{ export: string; receivers: Receiver[] }> }>;
}

/**
 * The listing as the lister gave it before `scope_receiver`: the field removed
 * at every depth. The golden holds no `resolved_file`, which names a temp directory.
 */
function withoutScopeReceivers<T>(value: T): T {
  return JSON.parse(JSON.stringify(value, (key, v) => (key === 'scope_receiver' || key === 'resolved_file' ? undefined : v)));
}

describe('list_library_surface: scope_receiver (carrick#2774 T1)', () => {
  let client: SidecarClient;
  let root: string;
  let requestId = 0;

  async function list(extra: Record<string, unknown> = {}): Promise<Listed> {
    const response = await client.send<Listed>(
      { request_id: `s-${requestId++}`, action: 'list_library_surface', from_dir: root, packages: ['fixture-chain-router'], ...extra },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response;
  }

  const receiverOf = (listed: Listed, id: string): Receiver =>
    listed.surfaces[0].exports.flatMap(e => e.receivers).find(r => r.receiver === id)!;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-scope-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      'node_modules/fixture-chain-router/package.json': JSON.stringify({ name: 'fixture-chain-router', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/fixture-chain-router/index.d.ts': ROUTER,
      'node_modules/fixture-foreign/package.json': JSON.stringify({ name: 'fixture-foreign', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/fixture-foreign/index.d.ts': FOREIGN,
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

  it('lists the object `at(path)` returns, as the verifier would read it', async () => {
    const app = receiverOf(await list(), 'instance:()');
    const at = app.members.find(m => m.name === 'at')!;
    assert.strictEqual(at.scope_receiver?.receiver, 'instance:()>scope:at');
    assert.deepStrictEqual(at.scope_receiver!.members.map(m => m.name), ['delete', 'get', 'post']);
  });

  it('lists an overloaded generic scope once, at the overload the build resolves', async () => {
    const app = receiverOf(await list(), 'instance:()');
    assert.strictEqual(app.members.filter(m => m.name === 'at2').length, 1);
    const at2 = app.members.find(m => m.name === 'at2')!;
    assert.strictEqual(at2.scope_receiver?.receiver, 'instance:()>scope:at2');
    assert.deepStrictEqual(at2.scope_receiver!.members.map(m => m.name), ['get']);
  });

  it('lists none for a member that returns this, a thenable, a promise, a primitive, two types, a function, no string slot, a foreign type or a sibling of the receiver', async () => {
    const app = receiverOf(await list(), 'instance:()');
    assert.deepStrictEqual(app.members.filter(m => m.scope_receiver).map(m => m.name).sort(), LISTED);
    assert.deepStrictEqual(app.members.filter(m => !m.scope_receiver).map(m => m.name).sort(), NOTHING);
  });

  it('lists none on the export receiver, and none inside a scope receiver', async () => {
    const listed = await list();
    assert.ok(receiverOf(listed, 'export').members.every(m => m.scope_receiver === undefined));
    for (const member of receiverOf(listed, 'instance:()').members) {
      for (const inner of member.scope_receiver?.members ?? []) assert.strictEqual(inner.scope_receiver, undefined);
    }
  });

  it('keeps the hash: it is the hash the lister gave before, and the hash of the listing without the field', async () => {
    const golden = JSON.parse(fs.readFileSync(GOLDEN, 'utf8'));
    const listed = await list();
    assert.strictEqual(listed.surface_sha256, golden.surface_sha256);
    assert.strictEqual(listed.surface_sha256, fullSurfaceSha256(listed.surfaces.map(surface => JSON.parse(JSON.stringify(surface, (key, v) => (key === 'scope_receiver' ? undefined : v))))));
  });

  it('draws on a cap of its own: a tiny max_entries gives the listing, and `truncated`, the lister gave before', async () => {
    const golden = JSON.parse(fs.readFileSync(GOLDEN, 'utf8'));
    const tiny = await list({ max_entries: golden.tiny.max_entries });
    assert.ok(golden.tiny.surfaces[0].truncated > 0, 'the golden cap cuts nothing');
    assert.deepStrictEqual(withoutScopeReceivers(tiny.surfaces), golden.tiny.surfaces);
    assert.strictEqual(tiny.surface_sha256, golden.tiny.surface_sha256);
  });

  it('lists no scope receiver for a package whose members return nothing to register on', async () => {
    // The fixture's foreign package has a `get` that returns `this`, and no scope member.
    const response = await client.send<Listed>(
      { request_id: 'foreign', action: 'list_library_surface', from_dir: root, packages: ['fixture-foreign'] },
      60_000
    );
    assert.ok(!JSON.stringify(response.surfaces).includes('scope_receiver'));
  });
});
