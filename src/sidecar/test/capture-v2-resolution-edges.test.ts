/**
 * carrick#2091: capture records how the scanned repo's installed tree
 * resolved what the stub's pins depend on. Hand-built node_modules trees, no
 * install: the walk reads them the way Node's lookup does.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import { installedResolutionEdges, RESOLUTION_FILE } from '../src/capture/resolution-edges.js';

function tempDir(): string {
  return fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-resolution-edges-')));
}

/** Write `<dir>/package.json` for an installed package. */
function pkg(dir: string, name: string, version: string, dependencies: Record<string, string> = {}): void {
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ name, version, dependencies }));
}

function link(target: string, at: string): void {
  fs.mkdirSync(path.dirname(at), { recursive: true });
  fs.symlinkSync(target, at, 'dir');
}

describe('installedResolutionEdges (#2091)', () => {
  it('reads a pnpm workspace: the service links into a root store whose packages find siblings', () => {
    const ws = tempDir();
    const service = path.join(ws, 'apps', 'orders');
    fs.mkdirSync(service, { recursive: true });
    const store = path.join(ws, 'node_modules', '.pnpm');
    const parentDir = path.join(store, '@s+parent@1.0.0', 'node_modules', '@s', 'parent');
    const childDir = path.join(store, 'child@1.2.0', 'node_modules', 'child');
    const leafDir = path.join(store, 'leaf@3.0.1', 'node_modules', 'leaf');
    pkg(parentDir, '@s/parent', '1.0.0', { child: '^1.0.0' });
    pkg(childDir, 'child', '1.2.0', { leaf: '^3.0.0' });
    pkg(leafDir, 'leaf', '3.0.1');
    // pnpm's sibling links: each store entry's node_modules holds its deps.
    link(childDir, path.join(store, '@s+parent@1.0.0', 'node_modules', 'child'));
    link(leafDir, path.join(store, 'child@1.2.0', 'node_modules', 'leaf'));
    link(parentDir, path.join(service, 'node_modules', '@s', 'parent'));

    const { edges, unrecorded } = installedResolutionEdges(service, { '@s/parent': '1.0.0' });
    assert.deepStrictEqual(edges, {
      '@s/parent@1.0.0': { child: '1.2.0' },
      'child@1.2.0': { leaf: '3.0.1' },
    });
    assert.strictEqual(unrecorded, 0);
  });

  it('reads a hoisted tree', () => {
    const repo = tempDir();
    pkg(path.join(repo, 'node_modules', 'parent'), 'parent', '2.0.0', { child: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'child'), 'child', '1.4.0');
    const { edges } = installedResolutionEdges(repo, { parent: '2.0.0' });
    assert.deepStrictEqual(edges, { 'parent@2.0.0': { child: '1.4.0' } });
  });

  it('a nested copy that differs from the hoisted one wins for its parent', () => {
    const repo = tempDir();
    pkg(path.join(repo, 'node_modules', 'parent'), 'parent', '2.0.0', { child: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'parent', 'node_modules', 'child'), 'child', '1.1.0');
    pkg(path.join(repo, 'node_modules', 'child'), 'child', '2.5.0');
    const { edges } = installedResolutionEdges(repo, { parent: '2.0.0' });
    assert.deepStrictEqual(edges, { 'parent@2.0.0': { child: '1.1.0' } });
  });

  it('leaves out a workspace member, an npm alias and an unpublished version', () => {
    const repo = tempDir();
    pkg(path.join(repo, 'node_modules', 'parent'), 'parent', '2.0.0', {
      member: 'workspace:*',
      aliased: 'npm:real@^1.0.0',
      local: '^0.0.0',
      kept: '^1.0.0',
    });
    pkg(path.join(repo, 'packages', 'member'), 'member', '1.0.0');
    link(path.join(repo, 'packages', 'member'), path.join(repo, 'node_modules', 'member'));
    pkg(path.join(repo, 'node_modules', 'aliased'), 'real', '1.0.0');
    pkg(path.join(repo, 'node_modules', 'local'), 'local', '0.0.0-use.local');
    pkg(path.join(repo, 'node_modules', 'kept'), 'kept', '1.0.0');
    const { edges, unrecorded } = installedResolutionEdges(repo, { parent: '2.0.0' });
    assert.deepStrictEqual(edges, { 'parent@2.0.0': { kept: '1.0.0' } });
    assert.strictEqual(unrecorded, 3);
  });

  it('a pin installed at another version starts no walk', () => {
    const repo = tempDir();
    pkg(path.join(repo, 'node_modules', 'parent'), 'parent', '2.1.0', { child: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'child'), 'child', '1.4.0');
    const { edges } = installedResolutionEdges(repo, { parent: '2.0.0' });
    assert.deepStrictEqual(edges, {});
  });

  it('records only what the pins reach', () => {
    const repo = tempDir();
    pkg(path.join(repo, 'node_modules', 'parent'), 'parent', '2.0.0', { child: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'child'), 'child', '1.4.0');
    pkg(path.join(repo, 'node_modules', 'other'), 'other', '5.0.0', { child: '^1.0.0' });
    const { edges } = installedResolutionEdges(repo, { parent: '2.0.0' });
    assert.deepStrictEqual(Object.keys(edges), ['parent@2.0.0']);
  });

  it('leaves out an edge two copies of one parent version resolve differently', () => {
    const repo = tempDir();
    pkg(path.join(repo, 'node_modules', 'a'), 'a', '1.0.0', { shared: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'b'), 'b', '1.0.0', { shared: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'shared'), 'shared', '1.0.0', { leaf: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'leaf'), 'leaf', '1.0.0');
    // b's own copy of shared@1.0.0 resolves leaf differently.
    pkg(path.join(repo, 'node_modules', 'b', 'node_modules', 'shared'), 'shared', '1.0.0', { leaf: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'b', 'node_modules', 'shared', 'node_modules', 'leaf'), 'leaf', '1.3.0');
    const { edges } = installedResolutionEdges(repo, { a: '1.0.0', b: '1.0.0' });
    assert.deepStrictEqual(edges, {
      'a@1.0.0': { shared: '1.0.0' },
      'b@1.0.0': { shared: '1.0.0' },
    });
  });

  it('is sorted and byte-stable across runs', () => {
    const repo = tempDir();
    pkg(path.join(repo, 'node_modules', 'zeta'), 'zeta', '1.0.0', { beta: '^1.0.0', alpha: '^1.0.0' });
    pkg(path.join(repo, 'node_modules', 'alpha'), 'alpha', '1.0.0');
    pkg(path.join(repo, 'node_modules', 'beta'), 'beta', '1.0.0', { alpha: '^1.0.0' });
    const first = JSON.stringify(installedResolutionEdges(repo, { zeta: '1.0.0' }));
    const second = JSON.stringify(installedResolutionEdges(repo, { zeta: '1.0.0' }));
    assert.strictEqual(first, second);
    const { edges } = JSON.parse(first) as { edges: Record<string, Record<string, string>> };
    assert.deepStrictEqual(Object.keys(edges), ['beta@1.0.0', 'zeta@1.0.0']);
    assert.deepStrictEqual(Object.keys(edges['zeta@1.0.0']), ['alpha', 'beta']);
  });
});

describe('capture_v2 writes the recorded edges into the stub (#2091)', () => {
  function writeTree(root: string, files: Record<string, string>): string {
    for (const [name, text] of Object.entries(files)) {
      const p = path.join(root, name);
      fs.mkdirSync(path.dirname(p), { recursive: true });
      fs.writeFileSync(p, text);
    }
    return root;
  }

  const TSCONFIG = JSON.stringify({
    compilerOptions: { target: 'ES2020', module: 'commonjs', strict: true, skipLibCheck: true, rootDir: 'src' },
    include: ['src'],
  });
  const ANCHOR = {
    kind: 'symbol',
    alias: 'Endpoint_shape_Response',
    symbol_name: 'ShapeResponse',
    source_file: 'src/service.ts',
    anchor_origin: 'llm-symbol',
  } as const;

  it('records the edge from the pinned package to what it installed', () => {
    const root = tempDir();
    const repoRoot = writeTree(path.join(root, 'repo'), {
      'tsconfig.json': TSCONFIG,
      'src/service.ts':
        "import type { Shape } from 'parent';\nexport interface ShapeResponse { shape: Shape }\n",
      'node_modules/parent/package.json': JSON.stringify({
        name: 'parent',
        version: '1.0.0',
        types: 'index.d.ts',
        dependencies: { child: '^1.0.0' },
      }),
      'node_modules/parent/index.d.ts': "export type { Shape } from 'child';\n",
      'node_modules/child/package.json': JSON.stringify({ name: 'child', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/child/index.d.ts': 'export type Shape = { a: string };\n',
    });
    const outDir = path.join(root, 'stub');
    const result = captureStub({ repoRoot, serviceName: 'edges-svc', outDir, anchors: [ANCHOR] });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.deepStrictEqual(result.pinned_dependencies, { parent: '1.0.0' });
    const recorded = JSON.parse(fs.readFileSync(path.join(result.stub_dir, RESOLUTION_FILE), 'utf8'));
    assert.deepStrictEqual(recorded, { edges: { 'parent@1.0.0': { child: '1.0.0' } } });
  });
});
