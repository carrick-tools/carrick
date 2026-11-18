/**
 * A workspace whose install is hoisted to the scan root and whose services are
 * subfolders: the install is the nearest `node_modules` at or above the
 * service, up to the scan root and no further. A service under such an install
 * is not a bare checkout, its installed versions are read from that install,
 * and the self-check resolves its external packages through it.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import { installedVersions } from '../src/capture/lockfile.js';
import { nearestNodeModules } from '../src/capture/installed-root.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'ES2020', module: 'commonjs', strict: true, skipLibCheck: true, rootDir: 'src' },
  include: ['src'],
});

const SERVICE_TS = "import { Item } from 'typedpkg';\nexport interface Resp {\n  item: Item;\n}\n";

const ANCHOR = {
  kind: 'symbol',
  alias: 'Endpoint_typed_Response',
  symbol_name: 'Resp',
  source_file: 'src/service.ts',
  anchor_origin: 'llm-symbol',
} as const;

function write(root: string, files: Record<string, string>): void {
  for (const [name, text] of Object.entries(files)) {
    const p = path.join(root, name);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, text);
  }
}

function typedPackage(version: string, field: string): Record<string, string> {
  return {
    'package.json': JSON.stringify({ name: 'typedpkg', version, types: 'index.d.ts' }),
    'index.d.ts': `export interface Item {\n  ${field}: string;\n}\n`,
  };
}

function withPrefix(prefix: string, files: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(files).map(([k, v]) => [`${prefix}${k}`, v]));
}

const SERVICE_FILES = {
  'package.json': JSON.stringify({ name: 'svc', dependencies: { typedpkg: '^1.0.0' } }),
  'tsconfig.json': TSCONFIG,
  'src/service.ts': SERVICE_TS,
};

interface Workspace {
  scan: string;
  stubRoot: string;
}

function workspace(files: Record<string, string>): Workspace {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-hoisted-install-'));
  const scan = path.join(base, 'ws');
  write(scan, files);
  return { scan, stubRoot: path.join(base, 'stub') };
}

function captureService(ws: Workspace, service: string) {
  const result = captureStub({
    repoRoot: path.join(ws.scan, service),
    scanRoot: ws.scan,
    serviceName: `${service}-svc`,
    outDir: path.join(ws.stubRoot, service),
    anchors: [ANCHOR],
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return result;
}

const HOISTED = {
  'package.json': JSON.stringify({ name: 'root', private: true, workspaces: ['svc-a', 'svc-b'] }),
  ...withPrefix('node_modules/typedpkg/', typedPackage('1.2.3', 'rootField')),
  ...withPrefix('svc-a/', SERVICE_FILES),
  ...withPrefix('svc-b/', SERVICE_FILES),
};

describe('capture v2: an install hoisted to the scan root', () => {
  it('does not read a subfolder service under a root install as bare', () => {
    const ws = workspace(HOISTED);
    for (const service of ['svc-a', 'svc-b']) {
      const result = captureService(ws, service);
      assert.strictEqual(result.bare_checkout, false, service);
      assert.deepStrictEqual(result.unresolved_dependencies, [], service);
    }
  });

  it('keeps the type a dependency in the root install gives, and self-checks it ok', () => {
    const ws = workspace(HOISTED);
    const result = captureService(ws, 'svc-a');
    const text = fs.readFileSync(path.join(result.stub_dir, 'types/service.d.ts'), 'utf8');
    assert.match(text, /item: Item;/);
    assert.doesNotMatch(text, /item: unknown/);
    assert.deepStrictEqual(result.aliases.map((a) => a.self_check), ['ok']);
  });

  it('reads a folder with no manifest of its own under a root install as installed, since its imports resolve through it', () => {
    const ws = workspace({
      ...HOISTED,
      ...withPrefix('functions/', { 'tsconfig.json': TSCONFIG, 'src/service.ts': SERVICE_TS }),
    });
    const result = captureService(ws, 'functions');
    assert.strictEqual(result.bare_checkout, false);
    assert.match(fs.readFileSync(path.join(result.stub_dir, 'types/service.d.ts'), 'utf8'), /item: Item;/);
  });

  it('reads a folder with no manifest and no install anywhere as bare', () => {
    const ws = workspace({ ...withPrefix('functions/', { 'tsconfig.json': TSCONFIG, 'src/service.ts': SERVICE_TS }) });
    assert.strictEqual(captureService(ws, 'functions').bare_checkout, true);
  });

  it('pins the version the root install holds', () => {
    const ws = workspace(HOISTED);
    assert.strictEqual(captureService(ws, 'svc-a').pinned_dependencies.typedpkg, '1.2.3');
    assert.strictEqual(installedVersions(path.join(ws.scan, 'svc-a'), ['typedpkg'], ws.scan).get('typedpkg'), '1.2.3');
  });

  it('lets a nested install win over the root install', () => {
    const ws = workspace({
      ...HOISTED,
      ...withPrefix('svc-a/node_modules/typedpkg/', typedPackage('9.9.9', 'ownField')),
    });
    assert.strictEqual(captureService(ws, 'svc-a').pinned_dependencies.typedpkg, '9.9.9');
    assert.strictEqual(captureService(ws, 'svc-b').pinned_dependencies.typedpkg, '1.2.3');
  });

  it('reads a workspace with no install anywhere as bare', () => {
    const ws = workspace({
      'package.json': JSON.stringify({ name: 'root', private: true }),
      ...withPrefix('svc-a/', SERVICE_FILES),
    });
    assert.strictEqual(captureService(ws, 'svc-a').bare_checkout, true);
  });

  it('never looks above the scan root', () => {
    const ws = workspace({ ...withPrefix('svc-a/', SERVICE_FILES) });
    // An install above the scan root belongs to something else.
    write(path.dirname(ws.scan), withPrefix('node_modules/typedpkg/', typedPackage('4.0.0', 'aboveField')));
    const service = path.join(ws.scan, 'svc-a');
    assert.strictEqual(nearestNodeModules(service, ws.scan), undefined);
    assert.strictEqual(captureService(ws, 'svc-a').bare_checkout, true);
    assert.strictEqual(installedVersions(service, ['typedpkg'], ws.scan).size, 0);
  });

  it('finds the nearest install and stops at the service that is the scan root', () => {
    const ws = workspace(HOISTED);
    assert.strictEqual(
      nearestNodeModules(path.join(ws.scan, 'svc-a'), ws.scan),
      path.join(ws.scan, 'node_modules')
    );
    assert.strictEqual(nearestNodeModules(ws.scan, ws.scan), path.join(ws.scan, 'node_modules'));
  });
});
