/**
 * An absolute specifier into an installed package becomes the package's bare
 * specifier in every stub file (carrick#1174).
 *
 * The v1 printer names a type that is not in scope by an absolute
 * `import("<checkout>/node_modules/.../index")`. Literal anchors carry that
 * text into the surface, and the specifier rewrite only knew how to map an
 * absolute path onto a file of the emitted tree. A path into an installed
 * package has no tree file, so it shipped as printed: the checkout root (or
 * the home directory, for a runtime's npm cache) inside a file the index
 * uploads, naming a module no other machine has.
 *
 * The rewrite now maps such a path to the package's public specifier plus
 * subpath (through its `exports` map when it has one, and the DefinitelyTyped
 * public name for `@types/*`), and pins the package it came from, so the check
 * workspace resolves the specifier against the pinned install.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord, CaptureStubResult } from '../src/capture/api.js';

function write(root: string, rel: string, text: string): void {
  const file = path.join(root, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

function link(root: string, target: string, rel: string): void {
  const file = path.join(root, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.symlinkSync(path.join(root, target), file, 'dir');
}

function stubFiles(dir: string): string[] {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory()
      ? stubFiles(path.join(dir, entry.name))
      : [path.join(dir, entry.name)]
  );
}

const STORE = 'node_modules/.pnpm';

describe('capture rewrites absolute installed-package specifiers (#1174)', () => {
  let scratch: string;
  let repoRoot: string;
  let cacheRoot: string;
  let result: CaptureStubResult;
  let records: Map<string, CaptureAliasRecord>;
  let surface: string;

  before(() => {
    scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-installed-spec-')));
    repoRoot = path.join(scratch, 'repo');
    cacheRoot = path.join(scratch, 'runtime-cache');

    write(repoRoot, 'tsconfig.json', JSON.stringify({
      compilerOptions: {
        target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler',
        strict: true, skipLibCheck: true, rootDir: 'src',
      },
      include: ['src'],
    }));
    write(repoRoot, 'package.json', JSON.stringify({
      name: 'svc', version: '0.0.0',
      dependencies: { widgetkit: '1.0.0', flatpkg: '4.5.6' },
    }));
    write(repoRoot, 'src/index.ts', 'export const ready = true;\n');

    // An isolated-store package with an exports map.
    const elementary = `${STORE}/elementary@2.3.4/node_modules/elementary`;
    write(repoRoot, `${elementary}/package.json`, JSON.stringify({
      name: 'elementary', version: '2.3.4',
      exports: {
        '.': { types: './index.d.ts', default: './index.js' },
        './nodes': { types: './sub/node.d.ts', default: './sub/node.js' },
      },
    }));
    write(repoRoot, `${elementary}/index.d.ts`,
      'export declare namespace JSX { interface Element { tag: string } }\n' +
      "export type { NodeRef } from './sub/node';\n" +
      "export type { Hidden } from './internal/hidden';\n");
    write(repoRoot, `${elementary}/sub/node.d.ts`, 'export interface NodeRef { id: string }\n');
    write(repoRoot, `${elementary}/internal/hidden.d.ts`, 'export interface Hidden { secret: string }\n');
    // Only the store's own link: the root cannot import it by name.
    link(repoRoot, elementary, `${STORE}/widgetkit@1.0.0/node_modules/elementary`);

    // A DefinitelyTyped package in the store.
    write(repoRoot, `${STORE}/@types+gizmo@1.2.0/node_modules/@types/gizmo/package.json`,
      JSON.stringify({ name: '@types/gizmo', version: '1.2.0', types: 'index.d.ts' }));
    write(repoRoot, `${STORE}/@types+gizmo@1.2.0/node_modules/@types/gizmo/index.d.ts`,
      'export interface Gizmo { size: number }\n');

    // A flat install without an exports map.
    write(repoRoot, 'node_modules/flatpkg/package.json',
      JSON.stringify({ name: 'flatpkg', version: '4.5.6', types: './lib/index.d.ts' }));
    write(repoRoot, 'node_modules/flatpkg/lib/index.d.ts', "export * from './thing';\n");
    write(repoRoot, 'node_modules/flatpkg/lib/thing.d.ts', 'export interface Thing { name: string }\n');

    // A runtime's npm cache outside the checkout: <registry>/<name>/<version>/.
    write(cacheRoot, 'npm/registry.example.org/cachedpkg/3.1.0/package.json', JSON.stringify({
      name: 'cachedpkg', version: '3.1.0', exports: { '.': { types: './dist/types.d.ts' } },
    }));
    write(cacheRoot, 'npm/registry.example.org/cachedpkg/3.1.0/dist/types.d.ts',
      'export interface Cached { hits: number }\n');

    const literal = (alias: string, type_text: string) => ({
      kind: 'literal' as const, alias, type_text, anchor_origin: 'deterministic-infer' as const,
    });
    result = captureStub({
      repoRoot,
      serviceName: 'installed-spec',
      outDir: path.join(scratch, 'stub'),
      anchors: [
        literal('A_root', `import("${repoRoot}/${elementary}/index").JSX.Element`),
        literal('A_subpath', `import("${repoRoot}/${elementary}/sub/node").NodeRef`),
        literal('A_reexported', `import("${repoRoot}/${elementary}/internal/hidden").Hidden`),
        literal('A_types', `{ gizmo: import("${repoRoot}/${STORE}/@types+gizmo@1.2.0/node_modules/@types/gizmo/index").Gizmo }`),
        literal('A_flat', `import("${repoRoot}/node_modules/flatpkg/lib/thing").Thing`),
        literal('A_cache', `import("${cacheRoot}/npm/registry.example.org/cachedpkg/3.1.0/dist/types").Cached`),
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    records = new Map(result.aliases.map((record) => [record.alias, record]));
    surface = fs.readFileSync(path.join(result.stub_dir, 'types/surface.d.ts'), 'utf8');
  });

  after(() => {
    fs.rmSync(scratch, { recursive: true, force: true });
  });

  it('leaves no absolute path in any stub file', () => {
    for (const file of stubFiles(result.stub_dir)) {
      const text = fs.readFileSync(file, 'utf8');
      assert.ok(!text.includes(scratch), `${path.relative(result.stub_dir, file)} holds ${scratch}:\n${text}`);
    }
  });

  it("names a package's root entry by the bare package name", () => {
    assert.match(surface, /A_root = import\("elementary"\)\.JSX\.Element;/);
  });

  it('names a subpath through the exports entry that serves it', () => {
    assert.match(surface, /A_subpath = import\("elementary\/nodes"\)\.NodeRef;/);
  });

  it('names an unexported file through the exported entry that re-exports the type', () => {
    // `internal/hidden` is not in the exports map, so `elementary/internal/hidden`
    // would not resolve against the install; the root entry re-exports it.
    assert.match(surface, /A_reexported = import\("elementary"\)\.Hidden;/);
  });

  it('names a DefinitelyTyped package by its public name and pins the types package', () => {
    assert.match(surface, /import\("gizmo"\)\.Gizmo/);
    assert.strictEqual(result.pinned_dependencies['@types/gizmo'], '1.2.0');
    assert.ok(!result.unpinned_externals.includes('gizmo'), JSON.stringify(result.unpinned_externals));
  });

  it('names a file of a package without exports by its subpath', () => {
    assert.match(surface, /A_flat = import\("flatpkg\/lib\/thing"\)\.Thing;/);
    assert.strictEqual(result.pinned_dependencies.flatpkg, '4.5.6');
  });

  it("maps a runtime cache path outside the checkout and pins the cache's version", () => {
    assert.match(surface, /A_cache = import\("cachedpkg"\)\.Cached;/);
    assert.strictEqual(result.pinned_dependencies.cachedpkg, '3.1.0');
    assert.strictEqual(result.pinned_dependencies.elementary, '2.3.4');
  });

  it('self-checks a rewritten specifier against the install it came from', () => {
    // None of these packages is importable by name from the checkout root, so
    // the producer's own node_modules cannot resolve the bare specifier. The
    // stub pins each one, which is what the check workspace resolves against;
    // the self-check must agree with it rather than fail the alias.
    for (const alias of ['A_root', 'A_subpath', 'A_reexported', 'A_types', 'A_flat', 'A_cache']) {
      const record = records.get(alias);
      assert.strictEqual(record?.self_check, 'ok', JSON.stringify(record));
    }
  });
});
