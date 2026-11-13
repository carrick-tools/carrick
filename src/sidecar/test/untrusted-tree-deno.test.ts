/**
 * carrick#4076: under CARRICK_UNTRUSTED_TREE the read bound admits the npm
 * package directories the scan's own Deno graph resolved, and nothing else
 * outside the checkout.
 *
 * A Deno service resolves `npm:` imports to Deno's npm cache, outside the
 * checkout. Before this, the bound refused those files and the loader's file
 * system threw ENOENT for the whole service. The bound now opens exactly the
 * `localPath` of each package the graph resolved, by real path, kept only when
 * strictly beneath the real npm cache Deno reports; and a file the bound
 * refuses (a climbing import, a tracked link, an outside `types` root) reads as
 * missing instead of failing the service.
 *
 * The fixture is generated in a temp base: `checkout/` (the service), `denodir/`
 * beside it (DENO_DIR for install and test), `outside/plain/secret.ts` (the
 * only place the sentinel lives), and capture stub dirs outside the checkout,
 * as the driver writes them. Test 1 is the known positive for every refusal.
 */

import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { ProjectLoader } from '../src/project-loader.js';
import { captureStub } from '../src/capture/index.js';
import { DenoProject, findDenoConfig, npmReadRoots } from '../src/capture/deno-project.js';
import { UNTRUSTED_TREE_ENV, realOf } from '../src/capture/checkout.js';

const hasDeno = spawnSync('deno', ['--version']).status === 0;
const SENTINEL = 'SENTINEL_4076_outside';
const HONO = 'hono@4.13.18';

const APP = [
  "import { Hono } from 'npm:hono@4.13.18';",
  'export const app = new Hono();',
  "app.get('/healthz', (c) => c.json({ ok: true }));",
  "app.post('/items', (c) => c.json({ id: 1 }));",
  '',
  'export type Ctx = Parameters<Parameters<typeof app.get>[1]>[0];',
  'export type AppT = typeof app;',
  '',
].join('\n');
const SIDE = [
  'import type { Secret } from "../../outside/plain/secret.ts";',
  'export declare const s: Secret;',
  'export type S = Secret;',
  '',
].join('\n');

function readTree(dir: string): Map<string, string> {
  const out = new Map<string, string>();
  const walk = (d: string): void => {
    for (const entry of fs.readdirSync(d, { withFileTypes: true })) {
      const abs = path.join(d, entry.name);
      if (entry.isDirectory()) walk(abs);
      else out.set(path.relative(dir, abs), fs.readFileSync(abs, 'utf8'));
    }
  };
  walk(dir);
  return out;
}

function enoent(fn: () => unknown): boolean {
  try {
    fn();
    return false;
  } catch (err) {
    return (err as { code?: string }).code === 'ENOENT';
  }
}

describe('carrick#4076: the untrusted bound admits the Deno npm packages the scan resolved', { skip: !hasDeno }, () => {
  let base: string;
  let denoDir: string;
  let npmCache: string;
  let honoDir: string;
  // One checkout per door: the main one holds the climbing import; the link
  // and the outside types root each fail the whole service alone on main.
  let checkout: string;
  let linkCheckout: string;
  let typesCheckout: string;
  let movedCheckout: string;
  let movedCache: string;
  // Files planted beside and inside the admitted package (test 6).
  let planted: Record<string, string>;
  let honoTypes: string;
  // Flag-off answers, read before any flag-on run (tests 2, 3).
  let appTextOff = '';
  let captureOff: Map<string, string>;
  const savedFlag = process.env[UNTRUSTED_TREE_ENV];
  const savedDenoDir = process.env.DENO_DIR;

  const service = (name: string, files: Record<string, string>, denoJson: unknown = {}): string => {
    const root = path.join(base, name);
    fs.mkdirSync(path.join(root, '.git'), { recursive: true });
    fs.mkdirSync(path.join(root, 'src'), { recursive: true });
    fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify(denoJson));
    for (const [rel, text] of Object.entries(files)) fs.writeFileSync(path.join(root, rel), text);
    return root;
  };
  const install = (root: string, entries: string[]): void => {
    const installed = spawnSync('deno', ['install', '--node-modules-dir=none', '--entrypoint', ...entries], {
      cwd: root, encoding: 'utf8', env: { ...process.env, DENO_DIR: denoDir, DENO_NO_UPDATE_CHECK: '1' },
    });
    assert.equal(installed.status, 0, installed.stderr);
  };
  const appText = (loader: ProjectLoader, root: string): string =>
    loader.getProject().getSourceFileOrThrow(path.join(root, 'src/app.ts'))
      .getVariableDeclarationOrThrow('app').getType().getText();
  const capture = (root: string, out: string, alias: string, source: string): ReturnType<typeof captureStub> => {
    fs.rmSync(out, { recursive: true, force: true });
    return captureStub({
      repoRoot: root, serviceName: 'cap', outDir: out,
      anchors: [{ kind: 'symbol', alias, source_file: source, symbol_name: alias, anchor_origin: 'llm-symbol' }],
    });
  };
  const flag = (on: boolean): void => {
    if (on) process.env[UNTRUSTED_TREE_ENV] = '1';
    else delete process.env[UNTRUSTED_TREE_ENV];
  };

  before(() => {
    base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4076-'));
    denoDir = path.join(base, 'denodir');
    npmCache = path.join(denoDir, 'npm');
    process.env.DENO_DIR = denoDir;
    fs.mkdirSync(path.join(base, 'outside', 'plain'), { recursive: true });
    const secret = path.join(base, 'outside', 'plain', 'secret.ts');
    fs.writeFileSync(secret, `export type Secret = "${SENTINEL}";\n`);

    checkout = service('checkout', { 'src/app.ts': APP, 'src/side.ts': SIDE });
    install(checkout, ['src/app.ts', 'src/side.ts']);
    linkCheckout = service('linkco', { 'src/app.ts': APP });
    fs.symlinkSync(secret, path.join(linkCheckout, 'src', 'link.ts'));
    install(linkCheckout, ['src/app.ts']);
    typesCheckout = service('typesco', { 'src/app.ts': APP }, { compilerOptions: { types: [secret] } });
    install(typesCheckout, ['src/app.ts']);
    // A tree that names its own npm cache and Deno dir: the engine never reads them.
    movedCache = path.join(base, 'treecache');
    movedCheckout = service('movedco', { 'src/app.ts': APP, '.npmrc': `cache=${movedCache}\n` },
      { denoDir: movedCache, cacheDir: movedCache });
    install(movedCheckout, ['src/app.ts']);

    honoDir = path.join(npmCache, 'registry.npmjs.org', 'hono', '4.13.18');
    honoTypes = path.join(honoDir, 'dist', 'types', 'index.d.ts');
    assert.ok(fs.existsSync(honoTypes), `${HONO} types not installed at ${honoTypes}`);
    planted = {
      linkInPackage: path.join(honoDir, 'escape.d.ts'),
      siblingPackage: path.join(npmCache, 'registry.npmjs.org', 'other', '1.0.0', 'index.d.ts'),
      denoRemote: path.join(denoDir, 'remote', 'x.ts'),
      npmCacheItself: path.join(npmCache, 'x.d.ts'),
      treeNamedCache: path.join(movedCache, 'registry.npmjs.org', 'hono', '4.13.18', 'index.d.ts'),
    };
    fs.symlinkSync(secret, planted.linkInPackage);
    for (const file of Object.values(planted).slice(1)) {
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, `export type Planted = "${SENTINEL}";\n`);
    }
  });

  after(() => {
    if (savedFlag === undefined) delete process.env[UNTRUSTED_TREE_ENV];
    else process.env[UNTRUSTED_TREE_ENV] = savedFlag;
    if (savedDenoDir === undefined) delete process.env.DENO_DIR;
    else process.env.DENO_DIR = savedDenoDir;
    fs.rmSync(base, { recursive: true, force: true });
  });

  it('1. switch unset: the known positives', () => {
    flag(false);
    const loader = new ProjectLoader({ repoRoot: checkout });
    assert.equal(loader.load().success, true);
    appTextOff = appText(loader, checkout);
    assert.match(appTextOff, /Hono/);
    const side = loader.getProject().getSourceFileOrThrow(path.join(checkout, 'src/side.ts'));
    assert.equal(side.getVariableDeclarationOrThrow('s').getType().getText(), `"${SENTINEL}"`);
    const fsHost = loader.getProject().getFileSystem();
    for (const file of Object.values(planted)) assert.match(fsHost.readFileSync(file), new RegExp(SENTINEL));

    const off = capture(checkout, path.join(base, 'stub-off'), 'Ctx', 'src/app.ts');
    assert.equal(off.success, true, off.errors.join('\n'));
    captureOff = readTree(off.stub_dir);
    assert.match(captureOff.get('package.json') ?? '', /"hono"/);

    const esc = capture(checkout, path.join(base, 'stubesc-off'), 'S', 'src/side.ts');
    assert.ok([...readTree(esc.stub_dir).values()].some((text) => text.includes(SENTINEL)),
      'switch unset: the capture publishes the outside type');
    // Switch unset, the graph admits nothing extra.
    assert.deepEqual(new DenoProject(findDenoConfig(checkout)!, checkout).readRoots ?? [], []);
  });

  it('2. switch set: the loader walks and reads the npm types the graph resolved', () => {
    flag(true);
    const loader = new ProjectLoader({ repoRoot: checkout });
    assert.equal(loader.load().success, true);
    assert.doesNotThrow(() => loader.getProject());
    assert.equal(appText(loader, checkout), appTextOff);
    const deno = new DenoProject(findDenoConfig(checkout)!, checkout);
    assert.deepEqual(deno.readRoots, [realOf(honoDir)]);
  });

  it('3. switch set: capture with the stub outside the checkout equals the switch-unset capture', () => {
    flag(true);
    const on = capture(checkout, path.join(base, 'stub-on'), 'Ctx', 'src/app.ts');
    assert.equal(on.success, true, on.errors.join('\n'));
    assert.deepEqual(readTree(on.stub_dir), captureOff);
  });

  it('4. switch set: an import that climbs out, a tracked link to outside, and an outside types root each read as missing, and the service still loads', async (t) => {
    flag(true);
    const doors: Array<[string, string]> = [['import', checkout], ['link', linkCheckout], ['types', typesCheckout]];
    for (const [door, root] of doors) await t.test(door, () => {
      const loader = new ProjectLoader({ repoRoot: root });
      assert.equal(loader.load().success, true, root);
      const project = loader.getProject();
      assert.equal(appText(loader, root), appTextOff, root);
      for (const file of project.getSourceFiles()) {
        assert.ok(!file.getFullText().includes(SENTINEL), `${root}: ${file.getFilePath()} holds the sentinel`);
      }
      if (root === checkout) {
        const s = project.getSourceFileOrThrow(path.join(root, 'src/side.ts')).getVariableDeclarationOrThrow('s').getType();
        assert.ok(s.isAny(), s.getText());
        assert.ok(!s.getText().includes(SENTINEL));
      }
    });
  });

  it('5. switch set: capture never publishes the outside type', () => {
    flag(true);
    const esc = capture(checkout, path.join(base, 'stubesc-on'), 'S', 'src/side.ts');
    const files = fs.existsSync(esc.stub_dir) ? [...readTree(esc.stub_dir).values()] : [];
    assert.ok(!files.some((text) => text.includes(SENTINEL)), 'the capture published the outside type');
    assert.ok(!JSON.stringify(esc).includes(SENTINEL));
  });

  it('6. switch set: escapes beside and inside the admitted package read ENOENT through the loader\'s own file system', () => {
    flag(true);
    for (const root of [checkout, movedCheckout]) {
      const loader = new ProjectLoader({ repoRoot: root });
      assert.equal(loader.load().success, true, root);
      const fsHost = loader.getProject().getFileSystem();
      assert.match(fsHost.readFileSync(honoTypes), /Hono/);
      for (const [name, file] of Object.entries(planted)) {
        assert.ok(enoent(() => fsHost.readFileSync(file)), `${root}: ${name} was readable`);
      }
    }
    // A tree that names its own cache moves nothing the engine admits.
    const moved = new DenoProject(findDenoConfig(movedCheckout)!, movedCheckout);
    assert.ok(moved.readRoots.length > 0);
    for (const root of moved.readRoots) assert.ok(root.startsWith(realOf(npmCache) + path.sep), root);
  });

  it('7. npmReadRoots admits only real package directories beneath the npm cache', () => {
    const cache = path.join(base, 'pure', 'npm');
    const inside = path.join(cache, 'registry.npmjs.org', 'pkg', '1.0.0');
    const away = path.join(base, 'pure', 'away');
    fs.mkdirSync(inside, { recursive: true });
    fs.mkdirSync(away, { recursive: true });
    const linked = path.join(cache, 'registry.npmjs.org', 'linked');
    fs.symlinkSync(away, linked);
    const roots = npmReadRoots({
      'pkg@1.0.0': { localPath: inside },
      cache: { localPath: cache },
      outside: { localPath: away },
      'linked@1.0.0': { localPath: linked },
      none: {},
    }, cache);
    assert.deepEqual(roots, [realOf(inside)]);
    assert.deepEqual(npmReadRoots({ 'pkg@1.0.0': { localPath: inside } }, undefined), []);
  });
});
