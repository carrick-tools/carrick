import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { loaderErrors, nativeErrors, scratch, writeShadow } from './held-shadow-support.js';

const hasDeno = spawnSync('deno', ['--version']).status === 0;

// Named `deno-project-*` so the heavy shard runs it, where Deno is installed (scripts/test-files.mjs).
describe('write_shadow on a Deno project', { skip: !hasDeno }, () => {
  it('(c) matches the loader\'s own-file errors, vendors the npm cache, and finds hono', () => {
    const repo = scratch('held-shadow-deno-repo-');
    fs.writeFileSync(
      path.join(repo, 'deno.json'),
      JSON.stringify({ imports: { hono: 'npm:hono@4.6.14' }, nodeModulesDir: 'none' })
    );
    fs.writeFileSync(
      path.join(repo, 'main.ts'),
      'import { Hono } from "hono";\nconst app = new Hono();\napp.get("/ping", (c) => c.json({ ok: true }));\nexport const bad: number = "x";\nexport default app;\n'
    );
    const installed = spawnSync('deno', ['install', '--node-modules-dir=none'], { cwd: repo, encoding: 'utf8' });
    assert.equal(installed.status, 0, installed.stderr);
    const out = scratch('held-shadow-deno-out-');
    const shadow = writeShadow(repo, out);
    assert.equal(shadow.hono, true);
    assert.deepEqual(shadow.own_files, ['main.ts']);
    assert.ok(shadow.vendored > 0);
    const expected = loaderErrors(repo);
    assert.deepEqual(expected, ['main.ts:4:TS2322']);
    assert.deepEqual(nativeErrors(shadow.tsconfig, path.join(out, 'w')), expected);
  });
});
