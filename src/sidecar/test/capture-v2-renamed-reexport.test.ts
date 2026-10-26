/**
 * A bundled package declares a type in a chunk file under a short internal
 * name and re-exports it from its public entry under another name
 * (`export { O as OmitKeys } from './chunk-hash.js'`). The v1 printer names the
 * type by the chunk's absolute path and internal name. The specifier rewrite
 * matches the entry's export by symbol identity, not by name, and writes the
 * name the entry exports.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import { installedPackageSpecifier } from '../src/capture/installed-package.js';
import type { CaptureAliasRecord, CaptureStubResult } from '../src/capture/api.js';

function write(root: string, rel: string, text: string): void {
  const file = path.join(root, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

const CHUNK = 'build/modern/chunk-AbC123';

function fixture(scratch: string, entryText: string, chunkText?: string): string {
  const repoRoot = path.join(scratch, 'repo');
  write(repoRoot, 'tsconfig.json', JSON.stringify({
    compilerOptions: {
      target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler',
      strict: true, skipLibCheck: true, rootDir: 'src',
    },
    include: ['src'],
  }));
  write(repoRoot, 'package.json', JSON.stringify({
    name: 'svc', version: '0.0.0', dependencies: { qcore: '5.0.0' },
  }));
  write(repoRoot, 'src/index.ts', 'export const ready = true;\n');
  const pkg = 'node_modules/qcore';
  write(repoRoot, `${pkg}/package.json`, JSON.stringify({
    name: 'qcore', version: '5.0.0',
    exports: { '.': { types: './build/modern/index.d.ts', default: './build/modern/index.js' } },
  }));
  write(repoRoot, `${pkg}/${CHUNK}.d.ts`, chunkText ??
    ('type O<T, K extends keyof T> = Omit<T, K>;\n' +
    'interface Y<T> { (input: T): Promise<T> }\n' +
    'export { O, Y };\n'));
  write(repoRoot, `${pkg}/build/modern/index.d.ts`, entryText);
  return repoRoot;
}

const literal = (alias: string, type_text: string) => ({
  kind: 'literal' as const, alias, type_text, anchor_origin: 'deterministic-infer' as const,
});

function capture(scratch: string, repoRoot: string): {
  result: CaptureStubResult; records: Map<string, CaptureAliasRecord>; surface: string;
} {
  const chunk = `${repoRoot}/node_modules/qcore/${CHUNK}`;
  const result = captureStub({
    repoRoot,
    serviceName: 'renamed-reexport',
    outDir: path.join(scratch, 'stub'),
    anchors: [
      literal('A_omit', `import("${chunk}").O<{ a: string; b: number }, "b">`),
      literal('A_fn', `import("${chunk}").Y<string>`),
    ],
  });
  assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
  return {
    result,
    records: new Map(result.aliases.map((record) => [record.alias, record])),
    surface: fs.readFileSync(path.join(result.stub_dir, 'types/surface.d.ts'), 'utf8'),
  };
}

describe('capture names a renamed re-export by the name the entry exports', () => {
  let scratch: string;

  before(() => {
    scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-renamed-reexport-')));
  });
  after(() => {
    fs.rmSync(scratch, { recursive: true, force: true });
  });

  it('writes the exported name and self-checks ok', () => {
    const root = path.join(scratch, 'renamed');
    const repoRoot = fixture(root, "export { O as OmitKeys, Y as FetchFn } from './chunk-AbC123.js';\n");
    const { result, records, surface } = capture(root, repoRoot);
    assert.match(surface, /A_omit = import\("qcore"\)\.OmitKeys<\{\s*a: string;\s*b: number;\s*\}, "b">;/);
    assert.match(surface, /A_fn = import\("qcore"\)\.FetchFn<string>;/);
    assert.strictEqual(result.pinned_dependencies.qcore, '5.0.0');
    for (const alias of ['A_omit', 'A_fn']) {
      assert.strictEqual(records.get(alias)?.self_check, 'ok', JSON.stringify(records.get(alias)));
    }
  });

  it('keeps the decay, naming the unexposed file, when no entry exports the declaration', () => {
    const root = path.join(scratch, 'hidden');
    const repoRoot = fixture(root, "export { Y as FetchFn } from './chunk-AbC123.js';\n");
    const { records } = capture(root, repoRoot);
    const record = records.get('A_omit');
    assert.strictEqual(record?.self_check, 'decayed_internal', JSON.stringify(record));
    assert.match(JSON.stringify(record), /no entry of 'qcore' exports a declaration from 'build\/modern\/chunk-AbC123/);
  });

  it('is unchanged when the entry exports the same name', () => {
    const root = path.join(scratch, 'same');
    const repoRoot = fixture(root, "export { O, Y } from './chunk-AbC123.js';\n");
    const { records, surface } = capture(root, repoRoot);
    assert.match(surface, /A_omit = import\("qcore"\)\.O</);
    assert.strictEqual(records.get('A_omit')?.self_check, 'ok');
  });

  it('installedPackageSpecifier returns the exported name', () => {
    const root = path.join(scratch, 'unit');
    const repoRoot = fixture(root, "export { O as OmitKeys, Y as FetchFn } from './chunk-AbC123.js';\n");
    const chunk = `${repoRoot}/node_modules/qcore/${CHUNK}`;
    const mapped = installedPackageSpecifier(chunk, 'O');
    assert.strictEqual(mapped?.specifier, 'qcore');
    assert.strictEqual(mapped?.exportName, 'OmitKeys');
    assert.strictEqual(installedPackageSpecifier(chunk, 'Y')?.exportName, 'FetchFn');
  });

  // The chunk declares the type under its public name and exports it under a
  // short one (`export { type OmitKeyof as O }`); the entry re-exports that
  // short name under the public name again.
  it('matches a chunk export whose local declaration name differs from its export name', () => {
    const root = path.join(scratch, 'local-name');
    const chunkText =
      'type OmitKeyof<T, K extends keyof T> = Omit<T, K>;\n' +
      'interface QueryFunction<T> { (input: T): Promise<T> }\n' +
      'export { type OmitKeyof as O, type QueryFunction as Y };\n';
    const repoRoot = fixture(
      root,
      "export { O as OmitKeyof, type Y as QueryFunction } from './chunk-AbC123.js';\n",
      chunkText
    );
    const { records, surface } = capture(root, repoRoot);
    assert.match(surface, /A_omit = import\("qcore"\)\.OmitKeyof</);
    assert.match(surface, /A_fn = import\("qcore"\)\.QueryFunction</);
    for (const alias of ['A_omit', 'A_fn']) {
      assert.strictEqual(records.get(alias)?.self_check, 'ok', JSON.stringify(records.get(alias)));
    }
  });
});
