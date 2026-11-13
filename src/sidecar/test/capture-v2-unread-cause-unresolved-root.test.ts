/**
 * carrick#3995 (T6): a located site whose type is itself a bare `any` because
 * the call it reads goes through an import that did not resolve says
 * `unresolved_import`, not `declared`. The emitted stub prints a genuine
 * `any` there, so only the anchor's source program can tell the two apart:
 * the same anchor-time path that labels such members (`unresolvedAtAnchor`)
 * now records the root as well.
 *
 * - `await pkg.get(...)` on an uninstalled package says `unresolved_import`,
 *   and so does a `.then` chained on it.
 * - A wrapper in the repo that states `Promise<unknown>` still says
 *   `declared`, even when its body calls the uninstalled package.
 * - An `any` a library declares (`JSON.parse`, `Response.json()`) still says
 *   `declared`.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord, InferAnchorRequest } from '../src/capture/api.js';

const roots: string[] = [];

after(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

const TSCONFIG = {
  compilerOptions: {
    strict: true,
    module: 'esnext',
    moduleResolution: 'bundler',
    target: 'es2022',
    lib: ['es2022', 'dom'],
    skipLibCheck: true,
  },
  include: ['src'],
};

// Line numbers matter: the anchors below name them.
const CLIENT_TS = `import pkg from 'fixture-absent-http';

export async function viaWrapper(): Promise<unknown> {
  return pkg.get('/wrapped');
}

export async function loadAll(raw: string, res: Response) {
  const item = await pkg.get('/x');
  const chained = await pkg.get('/y').then((r: { data: unknown }) => r.data);
  const wrapped = await viaWrapper();
  const parsed = JSON.parse(raw);
  const body = await res.json();
  return { item, chained, wrapped, parsed, body };
}
`;

function capture(anchors: InferAnchorRequest[]): Map<string, CaptureAliasRecord> {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3995-t6-'));
  roots.push(repoRoot);
  fs.mkdirSync(path.join(repoRoot, 'src'), { recursive: true });
  fs.writeFileSync(path.join(repoRoot, 'tsconfig.json'), JSON.stringify(TSCONFIG));
  fs.writeFileSync(path.join(repoRoot, 'src/client.ts'), CLIENT_TS);
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3995-t6-stub-'));
  roots.push(outDir);
  const result = captureStub({ repoRoot, serviceName: 'items', outDir, anchors });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return new Map(result.aliases.map((record) => [record.alias, record]));
}

const at = (alias: string, line: number, expression: string): InferAnchorRequest => ({
  kind: 'infer',
  alias,
  source_file: 'src/client.ts',
  anchor_origin: 'deterministic-infer',
  line_number: line,
  expression_text: expression,
});

describe('capture: a root top type from an unresolved import says unresolved_import (carrick#3995)', () => {
  const records = capture([
    at('Item_Response', 8, "pkg.get('/x')"),
    at('Chained_Response', 9, "pkg.get('/y').then((r: { data: unknown }) => r.data)"),
    at('Wrapped_Response', 10, 'viaWrapper()'),
    at('Parsed_Response', 11, 'JSON.parse(raw)'),
    at('Body_Response', 12, 'res.json()'),
  ]);
  const record = (alias: string): CaptureAliasRecord => {
    const found = records.get(alias);
    assert.ok(found, `no record for ${alias}`);
    return found!;
  };

  it('an awaited call on an uninstalled package says unresolved_import', () => {
    const r = record('Item_Response');
    assert.strictEqual(r.top_type_at_self_check, true);
    assert.strictEqual(r.unread_cause, 'unresolved_import');
  });

  it('a .then chained on that call says unresolved_import', () => {
    const r = record('Chained_Response');
    assert.strictEqual(r.top_type_at_self_check, true);
    assert.strictEqual(r.unread_cause, 'unresolved_import');
  });

  it('a repo wrapper that states Promise<unknown> still says declared', () => {
    const r = record('Wrapped_Response');
    assert.strictEqual(r.top_type_at_self_check, true);
    assert.strictEqual(r.unread_cause, 'declared');
  });

  it('an any a library declares still says declared', () => {
    for (const alias of ['Parsed_Response', 'Body_Response']) {
      const r = record(alias);
      assert.strictEqual(r.top_type_at_self_check, true, alias);
      assert.strictEqual(r.unread_cause, 'declared', alias);
    }
  });

  it('the root cause is not a member finding: any_provenance stays empty', () => {
    for (const alias of ['Item_Response', 'Chained_Response']) {
      assert.strictEqual(record(alias).any_provenance, undefined, alias);
    }
  });
});
