/**
 * A bare name in `compilerOptions.types` resolves the way Deno resolves it from
 * the service folder: the member's own import map, the workspace's, a root
 * `scopes` entry for the member, the member's package.json, the longest
 * matching `prefix/` key. Each case below lost its types file's globals when
 * the name was resolved by a hand-written lookup instead.
 *
 * The npm cases fetch a small package into Deno's cache, as the other npm
 * cases in deno-project.test.ts do, so they run where the network does.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { DenoProject, findDenoConfig } from '../src/capture/index.js';

const hasDeno = spawnSync('deno', ['--version']).status === 0;

/** Writes `files` (relative path -> text) under a fresh workspace root and returns it. */
function tree(files: Record<string, string>): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-types-')));
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  }
  return root;
}

/** The project for the `app` member, and the global-types files it picked up. */
function read(root: string): { globals: string[]; diagnostics: string[] } {
  const service = path.join(root, 'app');
  const project = new DenoProject(findDenoConfig(service)!, service);
  return { globals: project.globals.map(g => fs.realpathSync(g)), diagnostics: project.diagnostics };
}

/** Writes the lockfile the scan reads frozen, fetching the packages into Deno's cache. */
function install(root: string): void {
  const done = spawnSync('deno', ['install', '--node-modules-dir=none'], { cwd: root, encoding: 'utf8' });
  assert.equal(done.status, 0, done.stderr);
}

const main = { 'app/main.ts': 'export const row = { id: "a" };\n' };

describe('bare compilerOptions.types names resolve as Deno resolves them', { skip: !hasDeno }, () => {
  it('takes the longest matching prefix key, not the first listed', () => {
    const root = tree({
      'deno.json': JSON.stringify({ workspace: ['./app'], imports: { '@kit/': './short/', '@kit/web/': './long/' } }),
      'app/deno.json': JSON.stringify({ compilerOptions: { types: ['@kit/web/globals.d.ts'] } }),
      'short/web/globals.d.ts': 'interface ShortGlobal { id: string }',
      'long/globals.d.ts': 'interface LongGlobal { id: string }',
      ...main,
    });
    try {
      const { globals, diagnostics } = read(root);
      assert.deepEqual(diagnostics, []);
      assert.ok(globals.includes(path.join(root, 'long', 'globals.d.ts')), globals.join('\n'));
      assert.equal(globals.some(g => g.includes(`${path.sep}short${path.sep}`)), false);
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  it('honours a root `scopes` entry keyed to the member folder', () => {
    const root = tree({
      'deno.json': JSON.stringify({ workspace: ['./app'], scopes: { './app/': { '@scoped': './app/scoped.d.ts' } } }),
      'app/deno.json': JSON.stringify({ compilerOptions: { types: ['@scoped'] } }),
      'app/scoped.d.ts': 'interface ScopedGlobal { id: string }',
      ...main,
    });
    try {
      const { globals, diagnostics } = read(root);
      assert.deepEqual(diagnostics, []);
      assert.ok(globals.includes(path.join(root, 'app', 'scoped.d.ts')), globals.join('\n'));
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  it('resolves a subpath of an import-map key that has no trailing slash and maps to npm:', () => {
    const root = tree({
      'deno.json': JSON.stringify({ workspace: ['./app'] }),
      'app/deno.json': JSON.stringify({
        imports: { msTypes: 'npm:@types/ms@0.7.34' },
        compilerOptions: { types: ['msTypes/index.d.ts'] },
      }),
      ...main,
    });
    try {
      install(root);
      const { globals, diagnostics } = read(root);
      assert.deepEqual(diagnostics, []);
      assert.ok(globals.some(g => g.endsWith(`${path.sep}@types${path.sep}ms${path.sep}0.7.34${path.sep}index.d.ts`)), globals.join('\n'));
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  it('resolves a bare name from the member package.json dependencies', () => {
    const root = tree({
      'deno.json': JSON.stringify({ workspace: ['./app'] }),
      'app/deno.json': JSON.stringify({ compilerOptions: { types: ['@types/ms/index.d.ts'] } }),
      'app/package.json': JSON.stringify({ dependencies: { '@types/ms': '0.7.34' } }),
      ...main,
    });
    try {
      install(root);
      const { globals, diagnostics } = read(root);
      assert.deepEqual(diagnostics, []);
      assert.ok(globals.some(g => g.endsWith(`${path.sep}@types${path.sep}ms${path.sep}0.7.34${path.sep}index.d.ts`)), globals.join('\n'));
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  it('still reads an exact key, a relative path and a name Deno cannot resolve', () => {
    const root = tree({
      'deno.json': JSON.stringify({ workspace: ['./app'] }),
      'app/deno.json': JSON.stringify({
        imports: { '@exact': './exact.d.ts' },
        compilerOptions: { types: ['@exact', './rel.d.ts', 'nonesuch'] },
      }),
      'app/exact.d.ts': 'interface ExactGlobal { id: string }',
      'app/rel.d.ts': 'interface RelGlobal { id: string }',
      ...main,
    });
    try {
      const { globals, diagnostics } = read(root);
      assert.ok(globals.includes(path.join(root, 'app', 'exact.d.ts')), globals.join('\n'));
      assert.ok(globals.includes(path.join(root, 'app', 'rel.d.ts')), globals.join('\n'));
      assert.ok(diagnostics.some(d => d.includes('nonesuch')), 'an unresolvable name is reported, not dropped');
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });
});
