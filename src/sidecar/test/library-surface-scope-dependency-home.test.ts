/**
 * A scope member whose returned type is declared in a package the listed
 * package declares as a dependency lists a scope receiver, with the returned
 * type's members (a client whose `from(name)` returns a builder from its REST
 * sibling package). A return from a package the listed package does not
 * declare lists none. The scope receiver stays outside `surface_sha256`.
 *
 * Every package below is invented and its declarations are hand-written.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { fullSurfaceSha256 } from '../src/library-claims.js';
import type { LibrarySurface } from '../src/types.js';

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

const REST = `export declare class Builder {
  select(columns?: string): this;
  insert(values: object): this;
  upsert(values: object): this;
  update(values: object): this;
  delete(): this;
}
`;

const HOSTED = `import { Builder } from '@fixture/hosted-rest';
export declare class Client {
  from(table: string): Builder;
  schemaName(): string;
}
export declare function createClient(url: string): Client;
`;

interface Member {
  name: string;
  scope_receiver?: { receiver: string; members: Member[] };
}
interface Listed {
  status: string;
  errors?: string[];
  surface_sha256: string;
  surfaces: Array<LibrarySurface & { exports: Array<{ export: string; receivers: Array<{ receiver: string; members: Member[] }> }> }>;
}

describe('list_library_surface: scope receiver from a declared dependency', () => {
  let client: SidecarClient;
  let root: string;
  let requestId = 0;

  async function list(pkg: string): Promise<Listed> {
    const response = await client.send<Listed>(
      { request_id: `d-${requestId++}`, action: 'list_library_surface', from_dir: root, packages: [pkg] },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response;
  }

  const clientMembers = (listed: Listed): Member[] =>
    listed.surfaces[0].exports.flatMap(e => e.receivers).find(r => r.receiver === 'instance:()')!.members;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-depscope-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      'node_modules/@fixture/hosted-rest/package.json': JSON.stringify({ name: '@fixture/hosted-rest', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/@fixture/hosted-rest/index.d.ts': REST,
      'node_modules/@fixture/hosted/package.json': JSON.stringify({
        name: '@fixture/hosted',
        version: '1.0.0',
        types: 'index.d.ts',
        dependencies: { '@fixture/hosted-rest': '^1.0.0' },
      }),
      'node_modules/@fixture/hosted/index.d.ts': HOSTED,
      'node_modules/@fixture/hosted-bare/package.json': JSON.stringify({ name: '@fixture/hosted-bare', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/@fixture/hosted-bare/index.d.ts': HOSTED,
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

  it('lists the builder a declared dependency declares, with its members', async () => {
    const from = clientMembers(await list('@fixture/hosted')).find(m => m.name === 'from')!;
    assert.strictEqual(from.scope_receiver?.receiver, 'instance:()>scope:from');
    assert.deepStrictEqual(from.scope_receiver!.members.map(m => m.name), ['delete', 'insert', 'select', 'update', 'upsert']);
  });

  it('lists none when the package does not declare the package that holds the type', async () => {
    const members = clientMembers(await list('@fixture/hosted-bare'));
    assert.deepStrictEqual(members.filter(m => m.scope_receiver), []);
  });

  it('keeps surface_sha256 as the hash of the listing without the scope receiver', async () => {
    const listed = await list('@fixture/hosted');
    const bare = listed.surfaces.map(surface => JSON.parse(JSON.stringify(surface, (key, v) => (key === 'scope_receiver' ? undefined : v))));
    assert.strictEqual(listed.surface_sha256, fullSurfaceSha256(bare));
  });
});
