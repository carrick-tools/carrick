/**
 * carrick#3696: an instance receiver's own property that has no call
 * signature, and whose type has callable members, is listed in the
 * receiver's `properties` with those members, the way a procedure router
 * hangs its procedure builder off the made instance (`t.procedure.query`).
 * A claim reaches it as an op's `path: [name]`, so the receiver grammar is
 * unchanged. It has a budget of its own, counted in `properties_left_out`,
 * and is left out of `surface_sha256`, so the listing without it,
 * `truncated`, the handler type ids and the hash are the ones the lister
 * gave before.
 *
 * Every package below is invented and its declarations are hand-written.
 * `fixtures/library-surface-receiver-properties.golden.json` holds what the
 * lister of the commit before this change gave for the same package: the
 * hash, the listing with handlers, and the listing under a tiny
 * `max_entries`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { fullSurfaceSha256 } from '../src/library-claims.js';
import type { LibrarySurface, SurfaceReceiver } from '../src/types.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GOLDEN = path.join(here, '..', '..', 'test', 'fixtures', 'library-surface-receiver-properties.golden.json');

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

const FOREIGN = `export interface ForeignBase {
  tools: { run(): void };
}
`;

const ROOT = `import { ForeignBase } from 'fixture-foreign-base';
export interface Context { user: string }
export interface Procedure { readonly kind: string }
export type Resolver = (opts: { input: unknown; ctx: Context }) => unknown;
export interface Builder {
  query(resolver: Resolver): Procedure;
  mutation(resolver: Resolver): Procedure;
  subscription(resolver: Resolver): Procedure;
  input(schema: unknown): Builder;
}
export interface Plain {
  readonly name: string;
  readonly size: number;
}
export interface Root extends ForeignBase {
  procedure: Builder;
  helpers: Builder;
  router(routes: Record<string, Procedure>): unknown;
  middleware(fn: (opts: { ctx: Context }) => void): Builder;
  callable: Builder & ((label: string) => void);
  plain: Plain;
  label: string;
}
export interface Factory {
  create(): Root;
  shared: Builder;
}
export declare const initRoot: Factory;
`;

interface Listed {
  status: string;
  errors?: string[];
  surface_sha256: string;
  surfaces: LibrarySurface[];
}

/** The listing as the lister gave it before `properties`: the fields removed, with `resolved_file`, which names a temp directory. */
function withoutProperties<T>(value: T): T {
  return JSON.parse(
    JSON.stringify(value, (key, v) =>
      key === 'properties' || key === 'properties_left_out' || key === 'resolved_file' ? undefined : v
    )
  );
}

describe('list_library_surface: receiver properties (carrick#3696)', () => {
  let client: SidecarClient;
  let root: string;
  let requestId = 0;

  async function list(extra: Record<string, unknown> = {}, packages = ['fixture-procedure-root']): Promise<Listed> {
    const response = await client.send<Listed>(
      { request_id: `p-${requestId++}`, action: 'list_library_surface', from_dir: root, packages, ...extra },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response;
  }

  const receiverOf = (listed: Listed, id: string): SurfaceReceiver =>
    listed.surfaces[0].exports.flatMap(e => e.receivers).find(r => r.receiver === id)!;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-properties-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      'node_modules/fixture-procedure-root/package.json': JSON.stringify({
        name: 'fixture-procedure-root',
        version: '1.0.0',
        types: 'index.d.ts',
      }),
      'node_modules/fixture-procedure-root/index.d.ts': ROOT,
      'node_modules/fixture-foreign-base/package.json': JSON.stringify({
        name: 'fixture-foreign-base',
        version: '1.0.0',
        types: 'index.d.ts',
      }),
      'node_modules/fixture-foreign-base/index.d.ts': FOREIGN,
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

  it('lists `procedure` on the made instance, with the builder members a path op names', async () => {
    const made = receiverOf(await list(), 'instance:create');
    assert.deepStrictEqual(
      made.properties?.map(p => [p.name, p.own, p.type]),
      [
        ['helpers', true, 'Builder'],
        ['procedure', true, 'Builder'],
      ]
    );
    const procedure = made.properties!.find(p => p.name === 'procedure')!;
    assert.deepStrictEqual(procedure.members.map(m => m.name), ['input', 'mutation', 'query', 'subscription']);
    assert.ok(procedure.members.every(m => m.own && m.signatures.length > 0));
    // A property is no member: the members array is the one the lister gave before.
    assert.ok(!made.members.some(m => m.name === 'procedure'));
  });

  it('lists no property with a call signature, no callable members, a primitive type, or one another package declares', async () => {
    const made = receiverOf(await list(), 'instance:create');
    const names = (made.properties ?? []).map(p => p.name);
    for (const name of ['callable', 'plain', 'label', 'tools']) assert.ok(!names.includes(name), name);
  });

  it('lists none on the export receiver, and the property members carry no scope receiver or properties of their own', async () => {
    const listed = await list();
    assert.strictEqual(receiverOf(listed, 'export').properties, undefined);
    for (const property of receiverOf(listed, 'instance:create').properties ?? []) {
      for (const member of property.members) assert.strictEqual(member.scope_receiver, undefined);
      assert.ok(!('properties' in property));
    }
  });

  it('lists the members handlers under `handlers: true`, after every handler type the lister gave before', async () => {
    const golden = JSON.parse(fs.readFileSync(GOLDEN, 'utf8'));
    const listed = await list({ handlers: true });
    const withoutHandlerTypes = (surfaces: unknown) =>
      JSON.parse(JSON.stringify(surfaces, (key, v) => (key === 'handler_types' ? undefined : v)));
    assert.deepStrictEqual(withoutHandlerTypes(withoutProperties(listed.surfaces)), withoutHandlerTypes(golden.handlers.surfaces));
    const before = golden.handlers.surfaces[0].handler_types;
    const after = listed.surfaces[0].handler_types!;
    assert.deepStrictEqual(after.slice(0, before.length), before, 'the handler type ids the lister gave before do not move');
    const procedure = receiverOf(listed, 'instance:create').properties!.find(p => p.name === 'procedure')!;
    const query = procedure.members.find(m => m.name === 'query')!;
    assert.ok((query.handlers ?? []).length > 0, 'query lists its resolver');
    for (const handler of query.handlers!) {
      for (const param of handler.params) assert.ok(param.type_id > 0 && param.type_id <= after.length);
    }
  });

  it('keeps the hash: it is the hash the lister gave before, and the hash of the listing without the field', async () => {
    const golden = JSON.parse(fs.readFileSync(GOLDEN, 'utf8'));
    const listed = await list();
    assert.strictEqual(listed.surface_sha256, golden.surface_sha256);
    assert.strictEqual(listed.surface_sha256, fullSurfaceSha256(withoutProperties(listed.surfaces)));
  });

  it('draws on a budget of its own: a tiny max_entries gives the listing and `truncated` the lister gave before, and counts what it leaves out', async () => {
    const golden = JSON.parse(fs.readFileSync(GOLDEN, 'utf8'));
    const tiny = await list({ max_entries: golden.tiny.max_entries });
    assert.ok(golden.tiny.surfaces[0].truncated > 0, 'the golden cap cuts nothing');
    assert.deepStrictEqual(withoutProperties(tiny.surfaces), golden.tiny.surfaces);
    assert.strictEqual(tiny.surface_sha256, golden.tiny.surface_sha256);
    assert.ok((tiny.surfaces[0].properties_left_out ?? 0) > 0, 'the tiny budget leaves nothing out');
    const full = await list();
    assert.strictEqual(full.surfaces[0].properties_left_out, undefined);
  });

  it('lists no properties for a package with no made instance', async () => {
    const listed = await list({}, ['fixture-foreign-base']);
    assert.ok(!JSON.stringify(listed.surfaces).includes('"properties"'));
  });
});
