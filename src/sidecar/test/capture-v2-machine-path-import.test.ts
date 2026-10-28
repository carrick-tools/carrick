/**
 * A stub never names a module by a machine path (carrick#2943).
 *
 * A literal anchor's text can name a type by `import("<absolute path>").Name`
 * where the printer that wrote it found no specifier: a workspace sibling's
 * SOURCE reached through a `node_modules` link, for one. The declaration emit
 * does not carry that file (the compiler treats it as a library), and it is
 * neither in the stub's tree nor an installed package, so the specifier
 * rewrite left the path as printed. On the machine that ran the capture the
 * path resolved to the source; after upload it became `<checkout>/...` and
 * resolved nowhere. The same pair then read one way judged from the fresh
 * stub and another from the stored one.
 *
 * A path the tree cannot hold is now `unknown` at its own position, labelled
 * as a reference that did not resolve, and the rest of the type survives. The
 * stub reads the same wherever the checkout sits.
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

function treeOf(dir: string, base = dir): Map<string, string> {
  const out = new Map<string, string>();
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const abs = path.join(dir, entry.name);
    if (entry.isSymbolicLink()) continue;
    if (entry.isDirectory()) {
      for (const [rel, text] of treeOf(abs, base)) out.set(rel, text);
    } else {
      out.set(path.relative(base, abs), fs.readFileSync(abs, 'utf8'));
    }
  }
  return out;
}

/** A two-package workspace: `api` reads `@acme/shared` through a link. */
function buildCheckout(repoRoot: string): void {
  write(repoRoot, 'package.json', JSON.stringify({ name: 'mono', private: true }));
  write(repoRoot, 'packages/shared/package.json', JSON.stringify({
    name: '@acme/shared', version: '0.0.0', private: true, type: 'module',
    exports: { '.': { types: './src/index.ts', default: './src/index.ts' } },
  }));
  write(repoRoot, 'packages/shared/src/types.ts',
    'export interface Token { access: string; expires: number }\n');
  write(repoRoot, 'packages/shared/src/index.ts', "export * from './types.js';\n");
  write(repoRoot, 'packages/api/package.json', JSON.stringify({
    name: '@acme/api', version: '0.0.0', private: true, type: 'module',
    dependencies: { '@acme/shared': 'workspace:*' },
  }));
  write(repoRoot, 'packages/api/tsconfig.json', JSON.stringify({
    compilerOptions: {
      target: 'ES2022', module: 'NodeNext', moduleResolution: 'NodeNext',
      strict: true, skipLibCheck: true, rootDir: 'src',
    },
    include: ['src'],
  }));
  write(repoRoot, 'packages/api/src/token.ts', [
    "import type { Token } from '@acme/shared';",
    'export async function refresh(): Promise<Token> {',
    "  return { access: 'a', expires: 1 };",
    '}',
    '',
  ].join('\n'));
  // A workspace install: the service reaches its sibling through a link.
  fs.mkdirSync(path.join(repoRoot, 'packages/api/node_modules/@acme'), { recursive: true });
  fs.symlinkSync('../../../shared', path.join(repoRoot, 'packages/api/node_modules/@acme/shared'), 'dir');
}

function capture(repoRoot: string, outDir: string): CaptureStubResult {
  // The type text a printer writes when it can name the sibling's module only
  // by where it sits on this machine.
  const types = `${repoRoot}/packages/shared/src/types`;
  const literal = (alias: string, type_text: string) => ({
    kind: 'literal' as const, alias, type_text, anchor_origin: 'deterministic-infer' as const,
  });
  return captureStub({
    repoRoot: path.join(repoRoot, 'packages/api'),
    scanRoot: repoRoot,
    serviceName: 'api',
    outDir,
    anchors: [
      literal('A_whole', `import("${types}").Token`),
      literal('A_nested', `{ plan: { dropped: import("${types}").Token[]; count: number } }`),
      literal('A_clean', '{ ok: boolean }'),
    ],
  });
}

describe('capture: no machine path in a stub (#2943)', () => {
  let scratch: string;
  let first: CaptureStubResult;
  let second: CaptureStubResult;
  let records: Map<string, CaptureAliasRecord>;
  let surface: string;

  before(() => {
    scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-machine-path-')));
    // The same checkout at two places, as on two machines.
    buildCheckout(path.join(scratch, 'one/repo'));
    buildCheckout(path.join(scratch, 'elsewhere/two/repo'));
    first = capture(path.join(scratch, 'one/repo'), path.join(scratch, 'stub-one'));
    second = capture(path.join(scratch, 'elsewhere/two/repo'), path.join(scratch, 'stub-two'));
    assert.ok(first.success, `capture failed: ${JSON.stringify(first.errors)}`);
    assert.ok(second.success, `capture failed: ${JSON.stringify(second.errors)}`);
    records = new Map(first.aliases.map((record) => [record.alias, record]));
    surface = fs.readFileSync(path.join(first.stub_dir, 'types/surface.d.ts'), 'utf8');
  });

  after(() => {
    fs.rmSync(scratch, { recursive: true, force: true });
  });

  it('leaves no absolute path in any stub file', () => {
    for (const [rel, text] of treeOf(first.stub_dir)) {
      assert.ok(!text.includes(scratch), `${rel} holds ${scratch}:\n${text}`);
      assert.ok(!/import\s*\(\s*["']\//.test(text), `${rel} imports an absolute path:\n${text}`);
    }
  });

  it('writes the same stub wherever the checkout sits', () => {
    const one = treeOf(first.stub_dir);
    const two = treeOf(second.stub_dir);
    assert.deepStrictEqual([...one.keys()].sort(), [...two.keys()].sort());
    for (const [rel, text] of one) {
      assert.strictEqual(two.get(rel), text, `${rel} differs between the two checkouts`);
    }
  });

  it('types the unreachable reference unknown and keeps the rest of the shape', () => {
    assert.match(surface, /export type A_whole = unknown;/);
    assert.match(surface, /dropped: unknown\[\];/);
    assert.match(surface, /count: number;/);
    assert.match(surface, /export type A_clean = \{\s*ok: boolean;\s*\};/);
  });

  it('labels a member position as a reference that did not resolve, never as declared', () => {
    const nested = records.get('A_nested')!;
    const found = (nested.any_provenance ?? []).find((p) => p.path === 'plan.dropped<0>');
    assert.ok(found, `no provenance at 'plan.dropped<0>': ${JSON.stringify(nested)}`);
    assert.strictEqual(found!.reason, 'unresolved_import', JSON.stringify(found));
    const detail = found!.detail ?? '';
    assert.ok(!detail.includes(scratch), detail);
    assert.ok(detail.includes('packages/shared/src/types'), detail);
  });

  it('never publishes the replaced type as clean', () => {
    // A whole alias that was the reference is a top type and decays as one.
    assert.strictEqual(records.get('A_whole')!.self_check, 'decayed_internal');
    assert.strictEqual(records.get('A_whole')!.top_type_at_self_check, true);
    assert.strictEqual(records.get('A_nested')!.self_check, 'decayed_internal');
    assert.strictEqual(records.get('A_clean')!.self_check, 'ok');
  });
});
