/**
 * Where the check phase looks for its vendored pnpm and tsc (carrick#833).
 *
 * The check phase is the only thing that produces a type verdict, and it
 * spawns two executables to do it. A missing lookup does not crash: every pair
 * degrades to `unverifiable`, which in the output is indistinguishable from
 * the two types agreeing. So the lookup has to hold across every shape the
 * sidecar can arrive in, and each shape is pinned here rather than assumed.
 *
 * The shapes, and why each one exists:
 *  - checkout / release tarball: `src/sidecar/node_modules/.bin`, installed by
 *    the sidecar's own `npm ci`.
 *  - npm install, hoisted: the package is `node_modules/carrick/`, the sidecar
 *    a plain directory inside it (npm strips nested `node_modules` from a
 *    published tarball), and the bins are at the install root.
 *  - npm install, nested: the same, except a version conflict elsewhere in the
 *    tree made npm put this package's own copy under it.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { resolveVendoredBin } from '../src/capture/check.js';

let root: string;

/** Create `<dir>/node_modules/.bin/<name>` and return its path. */
function bin(dir: string, name: string): string {
  const binDir = path.join(dir, 'node_modules', '.bin');
  fs.mkdirSync(binDir, { recursive: true });
  const file = path.join(binDir, name);
  fs.writeFileSync(file, '#!/bin/sh\nexit 0\n');
  fs.chmodSync(file, 0o755);
  return file;
}

describe('vendored bin lookup: every shape the sidecar arrives in', () => {
  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-bin-lookup-'));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('a checkout resolves to the sidecar\'s own node_modules, not an ancestor', () => {
    const installRoot = path.join(root, 'checkout');
    const sidecar = path.join(installRoot, 'src', 'sidecar');
    fs.mkdirSync(sidecar, { recursive: true });
    const ancestor = bin(installRoot, 'pnpm');
    const own = bin(sidecar, 'pnpm');

    const found = resolveVendoredBin('pnpm', sidecar);
    assert.strictEqual(found, own, `must prefer its own copy over ${ancestor}`);
  });

  // The shape carrick#833 was reported on. Nothing between the bundle and the
  // install root has a node_modules at all.
  it('an npm install resolves to the bins hoisted above the package', () => {
    const installRoot = path.join(root, 'hoisted');
    const sidecar = path.join(installRoot, 'node_modules', 'carrick', 'sidecar');
    fs.mkdirSync(sidecar, { recursive: true });
    const hoisted = bin(installRoot, 'pnpm');

    assert.ok(
      !fs.existsSync(path.join(sidecar, 'node_modules')),
      'the shape under test has no node_modules beside the bundle'
    );
    assert.strictEqual(resolveVendoredBin('pnpm', sidecar), hoisted);
  });

  it('a nested install resolves to the package\'s own copy, not the root', () => {
    const installRoot = path.join(root, 'nested');
    const pkg = path.join(installRoot, 'node_modules', 'carrick');
    const sidecar = path.join(pkg, 'sidecar');
    fs.mkdirSync(sidecar, { recursive: true });
    bin(installRoot, 'tsc');
    const nested = bin(pkg, 'tsc');

    assert.strictEqual(resolveVendoredBin('tsc', sidecar), nested);
  });

  // The caller checks existence and degrades explicitly. What it must never do
  // is degrade because the walk answered with something that is not the bin.
  it('names the sidecar-local path when the walk finds nothing', () => {
    const sidecar = path.join(root, 'bare', 'sidecar');
    fs.mkdirSync(sidecar, { recursive: true });

    const found = resolveVendoredBin('pnpm', sidecar);
    assert.strictEqual(found, path.join(sidecar, 'node_modules', '.bin', 'pnpm'));
    assert.strictEqual(fs.existsSync(found), false);
  });
});
