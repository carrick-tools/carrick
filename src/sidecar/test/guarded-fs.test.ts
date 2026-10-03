/**
 * The write guard every sidecar write and delete goes through (carrick#1748).
 * It resolves the path the operation touches and refuses anything outside the
 * run's own roots.
 */

import { describe, it, beforeEach, afterEach } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { WriteGuard, WriteRefused } from '../src/capture/guarded-fs.js';

describe('WriteGuard (carrick#1748)', () => {
  let base: string;
  let root: string;
  let outside: string;

  beforeEach(() => {
    base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-guard-'));
    root = path.join(base, 'stub');
    outside = path.join(base, 'repo');
    fs.mkdirSync(root);
    fs.mkdirSync(outside);
    fs.writeFileSync(path.join(outside, 'source.ts'), 'export const a = 1;\n');
  });

  afterEach(() => {
    fs.rmSync(base, { recursive: true, force: true });
  });

  it('writes, copies and deletes inside a root', () => {
    const guard = WriteGuard.of({ dirs: [root] });
    guard.mkdir(path.join(root, 'types', 'deep'));
    guard.writeFile(path.join(root, 'types', 'deep', 'a.d.ts'), 'x');
    guard.copyFile(path.join(outside, 'source.ts'), path.join(root, 'copy.ts'));
    guard.unlink(path.join(root, 'copy.ts'));
    guard.remove(path.join(root, 'types'));
    assert.deepStrictEqual(fs.readdirSync(root), []);
  });

  it('refuses a write, a copy and a delete outside its roots', () => {
    const guard = WriteGuard.of({ dirs: [root] });
    assert.throws(() => guard.writeFile(path.join(outside, 'source.ts'), 'y'), WriteRefused);
    assert.throws(() => guard.copyFile(path.join(root, 'x'), path.join(outside, 'x')), WriteRefused);
    assert.throws(() => guard.unlink(path.join(outside, 'source.ts')), WriteRefused);
    assert.throws(() => guard.remove(outside), WriteRefused);
    assert.throws(() => guard.mkdir(path.join(outside, 'new')), WriteRefused);
    assert.strictEqual(fs.readFileSync(path.join(outside, 'source.ts'), 'utf8'), 'export const a = 1;\n');
    assert.deepStrictEqual(fs.readdirSync(outside), ['source.ts']);
  });

  it('refuses a path that climbs out of a root with ..', () => {
    const guard = WriteGuard.of({ dirs: [root] });
    assert.throws(() => guard.writeFile(path.join(root, '..', 'repo', 'source.ts'), 'y'), WriteRefused);
  });

  it('does not take a sibling that shares the root as a name prefix', () => {
    const sibling = `${root}2`;
    fs.mkdirSync(sibling);
    const guard = WriteGuard.of({ dirs: [root] });
    assert.throws(() => guard.writeFile(path.join(sibling, 'a'), 'y'), WriteRefused);
  });

  it('refuses a write through a link in the root that leads outside it', () => {
    // The capture self-check links the repo's node_modules into the stub
    // (carrick#1742): the link is the stub's, what it reaches is the repo's.
    fs.symlinkSync(outside, path.join(root, 'node_modules'), 'dir');
    const guard = WriteGuard.of({ dirs: [root] });
    assert.throws(() => guard.writeFile(path.join(root, 'node_modules', 'source.ts'), 'y'), WriteRefused);
    assert.throws(() => guard.writeFile(path.join(root, 'node_modules', 'new.ts'), 'y'), WriteRefused);
    assert.throws(() => guard.mkdir(path.join(root, 'node_modules', 'pkg')), WriteRefused);
    assert.strictEqual(guard.allowsWrite(path.join(root, 'node_modules', 'source.ts')), false);
    assert.deepStrictEqual(fs.readdirSync(outside), ['source.ts']);
  });

  it('refuses a write to a dangling link whose target lies outside', () => {
    fs.symlinkSync(path.join(outside, 'created.ts'), path.join(root, 'dangling.ts'));
    const guard = WriteGuard.of({ dirs: [root] });
    assert.throws(() => guard.writeFile(path.join(root, 'dangling.ts'), 'y'), WriteRefused);
    assert.strictEqual(fs.existsSync(path.join(outside, 'created.ts')), false);
  });

  it('removes a link in the root without touching what it points to', () => {
    const link = path.join(root, 'node_modules');
    fs.symlinkSync(outside, link, 'dir');
    const guard = WriteGuard.of({ dirs: [root] });
    guard.unlink(link);
    assert.strictEqual(fs.existsSync(link), false);
    assert.deepStrictEqual(fs.readdirSync(outside), ['source.ts']);
    guard.symlink(outside, link, 'dir');
    guard.remove(root);
    assert.strictEqual(fs.existsSync(root), false);
    assert.deepStrictEqual(fs.readdirSync(outside), ['source.ts']);
  });

  it('refuses a new link placed outside its roots', () => {
    const guard = WriteGuard.of({ dirs: [root] });
    assert.throws(() => guard.symlink(root, path.join(outside, 'link'), 'dir'), WriteRefused);
  });

  it('matches a root reached through a link and its real path alike', () => {
    const alias = path.join(base, 'alias');
    fs.symlinkSync(root, alias, 'dir');
    const guard = WriteGuard.of({ dirs: [alias] });
    guard.writeFile(path.join(root, 'real.ts'), 'x');
    guard.writeFile(path.join(alias, 'aliased.ts'), 'x');
    assert.deepStrictEqual(fs.readdirSync(root).sort(), ['aliased.ts', 'real.ts']);
  });

  it('refuses a root that is, or contains, a protected tree', () => {
    assert.throws(() => WriteGuard.of({ dirs: [outside], protect: [outside] }), WriteRefused);
    assert.throws(() => WriteGuard.of({ dirs: [base], protect: [outside] }), WriteRefused);
    // A root inside the scanned tree is fine: a runtime cache lives there.
    const cache = path.join(outside, '.carrick', 'deno');
    const guard = WriteGuard.of({ dirs: [cache], protect: [outside] });
    guard.mkdir(cache);
    guard.writeFile(path.join(cache, 'runtime.d.ts'), 'x');
    assert.throws(() => guard.writeFile(path.join(outside, 'source.ts'), 'y'), WriteRefused);
  });

  it('refuses a directory root elsewhere inside a protected tree unless a .carrick directory holds it (carrick#1768)', () => {
    // A sibling service in the same repo: a stub dir there would be emptied.
    const sibling = path.join(outside, 'web');
    fs.mkdirSync(sibling);
    fs.writeFileSync(path.join(sibling, 'page.ts'), 'export const b = 2;\n');
    assert.throws(() => WriteGuard.of({ dirs: [sibling], protect: [outside] }), /outside a \.carrick directory/);
    assert.throws(() => WriteGuard.of({ dirs: [path.join(outside, 'new', 'stub')], protect: [outside] }), WriteRefused);
    // `.carrick` itself holds the workspace's proposal, jobs and scan logs.
    assert.throws(() => WriteGuard.of({ dirs: [path.join(outside, '.carrick')], protect: [outside] }), WriteRefused);
    // Every way a guard gains a root is held to the rule.
    const guard = WriteGuard.of({ dirs: [root], protect: [outside] });
    assert.throws(() => guard.with({ dirs: [sibling] }), WriteRefused);
    // A root reached through a link outside the tree is judged where it lands.
    fs.symlinkSync(sibling, path.join(base, 'alias'), 'dir');
    assert.throws(() => WriteGuard.of({ dirs: [path.join(base, 'alias')], protect: [outside] }), WriteRefused);

    // Beneath a `.carrick` directory, at any depth, a root is Carrick's own.
    WriteGuard.of({ dirs: [path.join(outside, '.carrick', 'stub')], protect: [outside] });
    WriteGuard.of({ dirs: [path.join(outside, 'web', '.carrick', 'deno', 'abc')], protect: [outside] });
    // A file root is exempt: the surface entry has to sit inside rootDir.
    WriteGuard.of({ files: [path.join(sibling, '__carrick_surface__.ts')], protect: [outside] });
    assert.deepStrictEqual(fs.readdirSync(sibling), ['page.ts']);
  });

  it('reads the .carrick directory on the resolved path, not the path as given (carrick#1768)', () => {
    // A `.carrick` that is a link into the repo's sources names nothing of Carrick's.
    fs.mkdirSync(path.join(outside, 'src'));
    fs.symlinkSync(path.join(outside, 'src'), path.join(outside, '.carrick'), 'dir');
    assert.throws(() => WriteGuard.of({ dirs: [path.join(outside, '.carrick', 'stub')], protect: [outside] }), WriteRefused);
  });

  it('holds a single-file root to that file alone', () => {
    const entry = path.join(outside, '__carrick_surface__.ts');
    const guard = WriteGuard.of({ files: [entry], protect: [outside] });
    guard.writeFile(entry, 'export type A = 1;\n');
    guard.unlink(entry);
    assert.throws(() => guard.writeFile(path.join(outside, 'source.ts'), 'y'), WriteRefused);
    assert.throws(() => guard.writeFile(path.join(outside, '__carrick_surface__.d.ts'), 'y'), WriteRefused);
    assert.deepStrictEqual(fs.readdirSync(outside), ['source.ts']);
  });

  it('lets an existing directory through mkdir, since nothing is written', () => {
    const guard = WriteGuard.of({ files: [path.join(outside, 'entry.ts')] });
    guard.mkdir(outside);
    assert.throws(() => guard.mkdir(path.join(outside, 'missing')), WriteRefused);
    assert.strictEqual(fs.existsSync(path.join(outside, 'missing')), false);
  });

  it('narrows only to a directory it already covers', () => {
    const guard = WriteGuard.of({ dirs: [root] });
    const types = path.join(root, 'types');
    const narrow = guard.narrow(types);
    assert.strictEqual(narrow.allowsWrite(path.join(types, 'a.d.ts')), true);
    assert.strictEqual(narrow.allowsWrite(path.join(root, 'package.json')), false);
    assert.throws(() => guard.narrow(outside), WriteRefused);
  });

  it('extends with more roots and keeps refusing the protected tree', () => {
    const guard = WriteGuard.of({ dirs: [root], protect: [outside] }).with({ dirs: [path.join(base, 'cache')] });
    guard.mkdir(path.join(base, 'cache'));
    assert.throws(() => guard.with({ dirs: [base] }), WriteRefused);
  });

  it('places a subprocess only inside its roots', () => {
    const guard = WriteGuard.of({ dirs: [root] });
    guard.assertWithin(root);
    assert.throws(() => guard.assertWithin(outside), WriteRefused);
  });

  it('makes a fresh scratch directory and covers nothing else', () => {
    const { dir, guard } = WriteGuard.scratch('carrick-scratch-', base);
    assert.strictEqual(path.dirname(dir), base);
    assert.deepStrictEqual(fs.readdirSync(dir), []);
    guard.writeFile(path.join(dir, 'a'), 'x');
    assert.throws(() => guard.writeFile(path.join(root, 'a'), 'x'), WriteRefused);
    guard.remove(dir);
    assert.strictEqual(fs.existsSync(dir), false);
  });
});
