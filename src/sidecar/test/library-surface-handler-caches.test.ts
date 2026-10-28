/**
 * carrick#2977 (caching half of ticket A): `list_library_surface` with
 * `handlers: true` prints each type once per program and builds a handler
 * parameter's members once per package, across every specifier of that
 * package in one request. Neither cache may move a byte of any listing.
 *
 * Every package below is invented and its declarations are hand-written.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as crypto from 'node:crypto';
import { Project } from 'ts-morph';
import { SidecarClient } from './helpers.js';
import { LibraryClaimsVerifier, surfaceReadCounts } from '../src/library-claims.js';
import type { LibrarySurface } from '../src/types.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'es2020',
    module: 'commonjs',
    moduleResolution: 'node16',
    strict: true,
    esModuleInterop: true,
    skipLibCheck: true,
    types: [],
  },
  include: ['src/**/*.ts'],
});

const VERBS = ['all', 'delete', 'get', 'head', 'options', 'patch', 'post', 'put'];

// Many members and two specifiers hand the same parameter types, so each
// type's print and member list would be read again at every parameter.
const HUB_INDEX = `export interface Request {
  body: unknown;
  params: Record<string, string>;
  headers: { host: string; accept?: string; via?: 'a' | 'b' | 'c' };
  raw: { url: string; method: string };
  flags: Record<'fast' | 'slow', boolean>;
  get(name: string): string | undefined;
}
export interface Reply {
  send(body: unknown): void;
  status(code: number): this;
  header(name: string, value: string | number | readonly string[]): this;
}
export interface Context<S = {}> { request: Request; reply: Reply; state: S }
export type Handler = (request: Request, reply: Reply) => void;
export interface Router {
${VERBS.map(verb => `  ${verb}(path: string, handler: Handler): this;\n  ${verb}(path: string, options: { schema?: object }, handler: Handler): this;`).join('\n')}
  route(options: { url: string; handler: Handler }): this;
  use(middleware: (ctx: Context<{ user: string }>, next: () => Promise<void>) => unknown): this;
  listen(port: number): void;
}
export declare function router(): Router;
`;

const HUB_PLUGIN = `import type { Request, Reply, Handler, Context } from './index';
export interface Plugin {
${VERBS.map(verb => `  ${verb}(path: string, handler: Handler): this;`).join('\n')}
  hook(name: string, run: (request: Request, reply: Reply, ctx: Context) => void): this;
}
export declare function plugin(): Plugin;
`;

// A second package handing its own request type, listed in the same request.
const OTHER = `export interface Req { body: unknown; query: { page?: string } }
export interface App { get(path: string, handler: (req: Req) => void): this }
export declare function app(): App;
`;

/**
 * The listing's bytes, minus the fields that carry the fixture's temporary
 * directory (`resolved_file`) and the version the fixture writes.
 */
function listingSha(surfaces: LibrarySurface[]): string {
  const kept = surfaces.map(s => [s.package, s.truncated, s.handlers_truncated ?? null, s.exports]);
  return crypto.createHash('sha256').update(JSON.stringify(kept)).digest('hex');
}

// Taken from the lister of origin/main a65fe412, before the caches.
const HANDLERS_ON_SHA256 = 'e9e6a2c67345578544ca47016be3f2da55b374cdfdc89274bfe541f4983797b9';
const HANDLERS_OFF_SHA256 = 'c5bbf046ca81fbf42ac893b127d93c5ed26f416679ee381d099e563da9b03c70';
const SURFACE_ON_SHA256 = '46ac356f47fa8e4ebaaa561a16e0d2b5fbbfc69edf63d7a126ee1bda78fb7bbe';
const SURFACE_OFF_SHA256 = '46ac356f47fa8e4ebaaa561a16e0d2b5fbbfc69edf63d7a126ee1bda78fb7bbe';

const PACKAGES = ['fixture-hub', 'fixture-hub/plugin', 'fixture-other'];

function writeFixture(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-caches-'));
  const files: Record<string, string> = {
    'tsconfig.json': TSCONFIG,
    'package.json': JSON.stringify({ name: 'svc' }),
    'src/index.ts': 'export const service = 1;\n',
    'node_modules/fixture-hub/package.json': JSON.stringify({
      name: 'fixture-hub',
      version: '1.0.0',
      exports: {
        '.': { types: './index.d.ts', default: './index.js' },
        './plugin': { types: './plugin.d.ts', default: './plugin.js' },
      },
    }),
    'node_modules/fixture-hub/index.d.ts': HUB_INDEX,
    'node_modules/fixture-hub/plugin.d.ts': HUB_PLUGIN,
    'node_modules/fixture-other/package.json': JSON.stringify({ name: 'fixture-other', version: '1.0.0', types: 'index.d.ts' }),
    'node_modules/fixture-other/index.d.ts': OTHER,
  };
  for (const [rel, text] of Object.entries(files)) {
    const file = path.join(root, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  }
  return root;
}

describe('list_library_surface: handler caches (carrick#2977)', () => {
  let client: SidecarClient;
  let root: string;
  let requestId = 0;

  async function list(
    packages: string[],
    handlers: boolean
  ): Promise<{ surfaces: LibrarySurface[]; surface_sha256: string }> {
    const response = await client.send<{ status: string; surfaces: LibrarySurface[]; surface_sha256: string; errors?: string[] }>(
      { request_id: `c-${requestId++}`, action: 'list_library_surface', from_dir: root, packages, handlers },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response;
  }

  before(async () => {
    root = writeFixture();
    client = new SidecarClient();
    await client.start();
    const init = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('lists every byte the lister gave before the caches, handlers on and off', async () => {
    const on = await list(PACKAGES, true);
    const off = await list(PACKAGES, false);
    // The fixture must exercise the caches: shared parameters across specifiers.
    const params = JSON.stringify(on.surfaces).match(/"param":/g)?.length ?? 0;
    assert.ok(params > 40, `only ${params} handler parameters listed`);
    assert.deepStrictEqual(
      [listingSha(on.surfaces), on.surface_sha256, listingSha(off.surfaces), off.surface_sha256],
      [HANDLERS_ON_SHA256, SURFACE_ON_SHA256, HANDLERS_OFF_SHA256, SURFACE_OFF_SHA256]
    );
  });

  it('lists a specifier the same alone as after its package`s other specifiers', async () => {
    const batched = await list(PACKAGES, true);
    const alone = await list(['fixture-hub/plugin'], true);
    const other = await list(['fixture-other'], true);
    assert.deepStrictEqual(alone.surfaces[0], batched.surfaces[1]);
    assert.deepStrictEqual(other.surfaces[0], batched.surfaces[2]);
  });
});

describe('list_library_surface: each shared type read once (carrick#2977)', () => {
  let root: string;

  before(() => {
    root = writeFixture();
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const listed = (packages: string[], handlers: boolean) => {
    const verifier = new LibraryClaimsVerifier(new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') }));
    surfaceReadCounts.handlerMemberBuilds = 0;
    surfaceReadCounts.typePrints = 0;
    const answer = verifier.listSurface(root, packages, 1_000_000, {}, { handlers });
    return { answer, counts: { ...surfaceReadCounts } };
  };

  it('builds a shared parameter type`s members once across every specifier of a package', () => {
    const { answer, counts } = listed(['fixture-hub', 'fixture-hub/plugin'], true);
    const params = JSON.stringify(answer.surfaces).match(/"param":/g)?.length ?? 0;
    assert.ok(params > 40, `only ${params} handler parameters listed`);
    // Request, Reply, Context<{ user: string }> and Context<{}>: one build each.
    assert.strictEqual(counts.handlerMemberBuilds, 4);
  });

  it('prints a type once however many parameters and signatures hand it', () => {
    const one = listed(['fixture-hub'], true).counts.typePrints;
    const both = listed(['fixture-hub', 'fixture-hub/plugin'], true).counts.typePrints;
    // The plugin's members hand types the index already printed; only its
    // own receiver types and the `hook` member's are new (4 here; 75 when
    // every parameter and signature printed its type again).
    assert.ok(one > 0);
    assert.ok(both - one < 10, `${both - one} prints for the plugin specifier`);
    // Handlers on prints only the handler member types beyond what off
    // prints (18 here; 185 uncached).
    const off = listed(['fixture-hub', 'fixture-hub/plugin'], false).counts.typePrints;
    assert.ok(both - off < 25, `${both - off} prints for handler parameters`);
  });
});
