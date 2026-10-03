/**
 * A capture leaves the repo it scans exactly as it found it (carrick#1748).
 *
 * Each fixture drives the write paths a real capture takes: the surface entry
 * placed inside the repo's rootDir, the stub's `node_modules` link into the
 * repo, the paths rewrite, the dangling-import repair (reaching a workspace
 * package's TypeScript source, the carrick#1742 shape), and for Deno the
 * graph root and the runtime cache. Afterwards every file, directory and link
 * in the repo is byte-identical. The one place a scan may write inside a repo
 * is a Deno root's documented cache, `.carrick/deno`, and only there.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { captureStub } from '../src/capture/index.js';
import { ProjectLoader } from '../src/project-loader.js';

/** Every file, directory and link under `dir`: a content hash, `dir`, or the link target. */
function snapshot(dir: string): Record<string, string> {
  const out: Record<string, string> = {};
  const walk = (current: string) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const abs = path.join(current, entry.name);
      const rel = path.relative(dir, abs).split(path.sep).join('/');
      if (entry.isSymbolicLink()) {
        out[rel] = `link:${fs.readlinkSync(abs)}`;
      } else if (entry.isDirectory()) {
        out[`${rel}/`] = 'dir';
        walk(abs);
      } else {
        out[rel] = crypto.createHash('sha256').update(fs.readFileSync(abs)).digest('hex');
      }
    }
  };
  walk(dir);
  return out;
}

function writeTree(root: string, files: Record<string, string>): void {
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  }
}

const NPM_REPO: Record<string, string> = {
  'package.json': JSON.stringify({ name: 'guard-root', private: true }),
  'tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'ESNext',
      moduleResolution: 'Bundler',
      strict: true,
      skipLibCheck: true,
      declaration: true,
      baseUrl: '.',
      paths: { '@app/*': ['src/*'] },
    },
    include: ['src'],
  }),
  'src/money.ts': 'export interface Money { amount: number; currency: string }\n',
  'src/routes/orders.ts': [
    "import type { Money } from '@app/money';",
    "import type { Shared } from '@ws/shared';",
    "import type { Row } from 'generated-db-client';",
    "import type { Price } from 'priced-lib';",
    '',
    'export interface Order {',
    '  id: string;',
    '  total: Money;',
    '  shared: Shared;',
    '  row: Row;',
    '  price: Price;',
    '}',
    '',
  ].join('\n'),
  // A workspace package whose declarations ARE its TypeScript source, which
  // imports a module the checkout does not have (carrick#1742).
  'packages/shared/package.json': JSON.stringify({
    name: '@ws/shared',
    version: '1.0.0',
    types: './src/index.ts',
    exports: { '.': { types: './src/index.ts', default: './src/index.ts' } },
  }),
  'packages/shared/src/index.ts': [
    "import type { Row } from 'generated-db-client';",
    '',
    'export interface Shared {',
    '  id: string;',
    '  row: Row;',
    '}',
    '',
  ].join('\n'),
  // An installed package, so the stub pins it and links node_modules.
  'node_modules/priced-lib/package.json': JSON.stringify({ name: 'priced-lib', version: '1.2.3', types: 'index.d.ts' }),
  'node_modules/priced-lib/index.d.ts': 'export interface Price { cents: number }\n',
};

const ROOTDIR_REPO: Record<string, string> = {
  'package.json': JSON.stringify({ name: 'rootdir-root', private: true }),
  'tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'ESNext',
      moduleResolution: 'Bundler',
      strict: true,
      skipLibCheck: true,
      rootDir: 'src',
    },
    include: ['src'],
  }),
  'src/user.ts': 'export interface User { id: string; name: string }\n',
};

describe('carrick#1748: a capture leaves an npm repo byte-identical', () => {
  let base: string;

  before(() => {
    base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1748-npm-'));
  });

  after(() => {
    fs.rmSync(base, { recursive: true, force: true });
  });

  it('links, rewrites paths and repairs inside the stub, never the repo', () => {
    const repoRoot = path.join(base, 'repo');
    writeTree(repoRoot, NPM_REPO);
    // The workspace link a package manager makes.
    fs.mkdirSync(path.join(repoRoot, 'node_modules', '@ws'), { recursive: true });
    fs.symlinkSync('../../packages/shared', path.join(repoRoot, 'node_modules', '@ws', 'shared'), 'dir');
    const before_ = snapshot(repoRoot);

    const result = captureStub({
      repoRoot,
      serviceName: 'orders',
      outDir: path.join(base, 'stub'),
      anchors: [
        {
          kind: 'symbol',
          alias: 'Endpoint_orders_Response',
          symbol_name: 'Order',
          source_file: 'src/routes/orders.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    // The write paths this fixture is here to drive did run.
    assert.ok(result.specifier_rewrites > 0, 'no specifier was rewritten');
    assert.strictEqual(result.pinned_dependencies['priced-lib'], '1.2.3');
    assert.strictEqual(fs.existsSync(path.join(result.stub_dir, 'node_modules')), false, 'the node_modules link was left in the stub');

    assert.deepStrictEqual(snapshot(repoRoot), before_);
  });

  it('places the surface entry inside rootDir and takes it away again', () => {
    const repoRoot = path.join(base, 'rootdir');
    writeTree(repoRoot, ROOTDIR_REPO);
    const before_ = snapshot(repoRoot);

    const result = captureStub({
      repoRoot,
      serviceName: 'users',
      outDir: path.join(base, 'rootdir-stub'),
      anchors: [
        {
          kind: 'symbol',
          alias: 'Endpoint_user_Response',
          symbol_name: 'User',
          source_file: 'src/user.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    assert.deepStrictEqual(snapshot(repoRoot), before_);
  });

  it('refuses an out_dir that would hold the repo, and deletes nothing', () => {
    const repoRoot = path.join(base, 'held');
    writeTree(repoRoot, ROOTDIR_REPO);
    const before_ = snapshot(repoRoot);

    // A stub dir is emptied before it is written; one that is, or holds, the
    // repo would take the repo with it.
    for (const outDir of [repoRoot, base]) {
      const result = captureStub({
        repoRoot,
        serviceName: 'users',
        outDir,
        anchors: [
          {
            kind: 'symbol',
            alias: 'Endpoint_user_Response',
            symbol_name: 'User',
            source_file: 'src/user.ts',
            anchor_origin: 'llm-symbol',
          },
        ],
      });
      assert.strictEqual(result.success, false);
      assert.match(result.errors.join('\n'), /carrick#1748/);
      assert.deepStrictEqual(snapshot(repoRoot), before_);
    }
  });
});

describe('carrick#1768: a capture writes nowhere else in the scanned repo', () => {
  let base: string;
  let mono: string;
  const USER_ANCHOR = {
    kind: 'symbol' as const,
    alias: 'Endpoint_user_Response',
    symbol_name: 'User',
    source_file: 'src/user.ts',
    anchor_origin: 'llm-symbol' as const,
  };

  before(() => {
    base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1768-'));
    mono = path.join(base, 'mono');
    writeTree(mono, {
      'package.json': JSON.stringify({ name: 'mono', private: true, workspaces: ['apps/*'] }),
      ...Object.fromEntries(Object.entries(ROOTDIR_REPO).map(([rel, text]) => [`apps/api/${rel}`, text])),
      'apps/web/package.json': JSON.stringify({ name: 'web', private: true }),
      'apps/web/src/page.ts': 'export const page = "home";\n',
    });
  });

  after(() => {
    fs.rmSync(base, { recursive: true, force: true });
  });

  it('refuses an out_dir in a sibling service, and deletes nothing', () => {
    const before_ = snapshot(mono);
    // The service is apps/api; the scan is the whole repo. A stub dir is
    // emptied before it is written, so apps/web would go with it.
    for (const outDir of [path.join(mono, 'apps', 'web'), path.join(mono, '.carrick')]) {
      const result = captureStub({
        repoRoot: path.join(mono, 'apps', 'api'),
        scanRoot: mono,
        serviceName: 'api',
        outDir,
        anchors: [USER_ANCHOR],
      });
      // The repo first: a leak shows as what it deleted.
      assert.deepStrictEqual(snapshot(mono), before_);
      assert.strictEqual(result.success, false, `captured into ${outDir}`);
      assert.match(result.errors.join('\n'), /outside a \.carrick directory/);
    }
  });

  it('captures into a .carrick directory inside the repo and leaves the rest as it was', () => {
    const before_ = snapshot(mono);
    const result = captureStub({
      repoRoot: path.join(mono, 'apps', 'api'),
      scanRoot: mono,
      serviceName: 'api',
      outDir: path.join(mono, 'apps', 'api', '.carrick', 'stub'),
      anchors: [USER_ANCHOR],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    const after_ = snapshot(mono);
    assert.deepStrictEqual(
      Object.fromEntries(Object.keys(before_).map((rel) => [rel, after_[rel]])),
      before_
    );
    assert.deepStrictEqual(
      Object.keys(after_).filter((rel) => !(rel in before_) && !rel.startsWith('apps/api/.carrick/')),
      []
    );
    fs.rmSync(path.join(mono, 'apps', 'api', '.carrick'), { recursive: true, force: true });
  });
});

const hasDeno = spawnSync('deno', ['--version']).status === 0;

describe('carrick#1748: a Deno capture writes only its documented cache', { skip: !hasDeno }, () => {
  let base: string;

  before(() => {
    base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1748-deno-'));
  });

  after(() => {
    fs.rmSync(base, { recursive: true, force: true });
  });

  it('leaves every file outside the two .carrick/deno roots byte-identical', () => {
    const root = path.join(base, 'repo');
    writeTree(root, {
      'deno.jsonc': `{
        "workspace": ["./app", "./lib"],
        "imports": { "@root/": "./shared/" },
        "compilerOptions": { "types": ["./global.d.ts"] },
      }`,
      'global.d.ts': 'interface DomainGlobal { status: "configured" }\n',
      'shared/model.ts': 'export interface Model { id: string; amount: number }\n',
      'lib/deno.json': JSON.stringify({ name: '@sample/lib', version: '1.0.0', exports: './mod.ts' }),
      'lib/mod.ts': 'export const remote = 123;\n',
      'app/deno.json': JSON.stringify({ imports: { '@local': './value.ts' } }),
      'app/value.ts': 'export const value = "local";\n',
      'app/main.ts': [
        "import type { Model } from '@root/model.ts';",
        "import { remote } from '@sample/lib';",
        "import { value } from '@local';",
        'export function response(): Model { return { id: value, amount: remote }; }',
        'export type Configured = DomainGlobal;',
        'export const env = Deno.env.get("URL");',
        '',
      ].join('\n'),
    });
    const service = path.join(root, 'app');
    // The workspace root keeps the runtime cache; the service keeps the graph
    // root, so its import map scopes the graph. Nothing else is Carrick's.
    const cacheRoots = ['.carrick/deno', 'app/.carrick/deno'];
    const holders = ['.carrick/', 'app/.carrick/'];
    const isCache = (rel: string) =>
      holders.includes(rel) || cacheRoots.some((cache) => rel === `${cache}/` || rel.startsWith(`${cache}/`));
    const before_ = snapshot(root);

    // The init'd project builds a Deno graph too (infer, bundle).
    assert.ok(new ProjectLoader({ repoRoot: service }).load().success);
    const result = captureStub({
      repoRoot: service,
      serviceName: 'app',
      outDir: path.join(base, 'stub'),
      anchors: [
        { kind: 'handler_return', alias: 'Response', symbol_name: 'response', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'Configured', symbol_name: 'Configured', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    assert.ok(result.aliases.every((a) => a.self_check === 'ok'), JSON.stringify(result.aliases));

    const after_ = snapshot(root);
    // Nothing that was there changed or went away.
    assert.deepStrictEqual(
      Object.fromEntries(Object.keys(before_).map((rel) => [rel, after_[rel]])),
      before_
    );
    // The only new entries are the cache roots, what they hold, and `.carrick/`.
    assert.deepStrictEqual(
      Object.keys(after_).filter((rel) => !(rel in before_) && !isCache(rel)),
      []
    );
    assert.ok(Object.keys(after_).some((rel) => rel.startsWith('.carrick/deno/')), 'the fixture built no Deno cache');
  });
});
