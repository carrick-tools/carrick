/**
 * A construct signature of an abstract class is listed with `abstract: true`:
 * `new` on it is a type error, so no claim may name it as a maker. A concrete
 * class and a construct signature written in an interface carry no such key.
 *
 * The package is invented and its declarations are hand-written.
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

const DECLARATIONS = `export declare abstract class Driver {
  constructor(options: { name: string });
  start(): void;
}
export declare abstract class BareDriver {
  start(): void;
}
export declare class Server {
  constructor(options: { name: string });
  start(): void;
}
export interface ServerConstructor {
  new (options: { name: string }): Server;
}
export declare const Made: ServerConstructor;
`;

const P = 'fixture-abstract';

interface Signature {
  abstract?: boolean;
  params: Array<{ name: string }>;
}
interface Surface {
  exports: Array<{ export: string; receivers: Array<{ receiver: string; construct?: Signature[] }> }>;
}

describe('list_library_surface: abstract construct signatures', () => {
  let client: SidecarClient;
  let root: string;
  let listed: Surface;
  let all: { surfaces: LibrarySurface[]; surface_sha256: string };

  const constructs = (exported: string): Signature[] => {
    const entry = listed.exports.find(e => e.export === exported);
    assert.ok(entry, `${exported} is not listed`);
    return entry.receivers.flatMap(r => r.construct ?? []);
  };

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-abstract-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
      [`node_modules/${P}/package.json`]: JSON.stringify({ name: P, version: '1.0.0', types: 'index.d.ts' }),
      [`node_modules/${P}/index.d.ts`]: DECLARATIONS,
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
    const response = await client.send<{ status: string; surfaces: Surface[]; surface_sha256: string; errors?: string[] }>(
      { request_id: 's', action: 'list_library_surface', from_dir: root, packages: [P] },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    listed = response.surfaces[0];
    all = response as unknown as { surfaces: LibrarySurface[]; surface_sha256: string };
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('marks the written construct signature of an abstract class', () => {
    const signatures = constructs('Driver');
    assert.strictEqual(signatures.length, 1);
    assert.strictEqual(signatures[0].abstract, true);
  });

  it('marks the implicit construct signature of an abstract class', () => {
    const signatures = constructs('BareDriver');
    assert.strictEqual(signatures.length, 1);
    assert.strictEqual(signatures[0].abstract, true);
  });

  it('leaves a concrete class without the key', () => {
    const signatures = constructs('Server');
    assert.strictEqual(signatures.length, 1);
    assert.ok(!('abstract' in signatures[0]), JSON.stringify(signatures[0]));
  });

  it('leaves a construct signature of an interface without the key', () => {
    const signatures = constructs('Made');
    assert.strictEqual(signatures.length, 1);
    assert.ok(!('abstract' in signatures[0]), JSON.stringify(signatures[0]));
  });

  it('keeps the hash: the listing hashes as it does without the field, so no package key moves (David 2026-11-15: a lister move never pays to rebuild every package)', () => {
    const without = JSON.parse(JSON.stringify(all.surfaces, (key, value) => (key === 'abstract' ? undefined : value)));
    assert.strictEqual(all.surface_sha256, fullSurfaceSha256(without));
  });
});
