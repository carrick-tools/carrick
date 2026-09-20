/**
 * The capture self-check blamed every alias for one alias's dangling import
 * (carrick-cloud#1184, cause 4).
 *
 * `self-check.ts` collected failed module specifiers per FILE, and every
 * alias's closure starts at `surface.d.ts` — which holds EVERY alias. So a
 * single dangling specifier anywhere in the surface marked the whole service
 * `decayed_internal`: two real services reported `usable_rate: 0` and
 * `by_self_check: { ok: 0 }` on 52 and 12 aliases while only a handful of
 * surface lines carried a diagnostic.
 *
 * That is not only a wrong metric. `backfill_accepted` (Rust) requires
 * `self_check == "ok"`, so on any service whose surface has one dangling line
 * every anchor-backfill re-capture was rejected.
 *
 * `check-poison.ts` has always attributed a surface diagnostic to the alias
 * whose `export type` statement SPAN covers it. This pins the capture side to
 * the same rule: a failure on the surface file blames the alias whose own
 * statement names the specifier, and nobody else.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord, CaptureStubResult } from '../src/capture/api.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'ES2020',
    module: 'commonjs',
    strict: true,
    declaration: true,
    rootDir: './',
  },
  include: ['**/*.ts'],
});

describe('capture v2: a surface diagnostic blames only its own alias (cloud#1184)', () => {
  let result: CaptureStubResult;
  let byAlias: Map<string, CaptureAliasRecord>;
  let outRoot: string;

  before(() => {
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-1184-'));
    const repoRoot = path.join(outRoot, 'repo');
    fs.mkdirSync(path.join(repoRoot, 'src'), { recursive: true });
    fs.writeFileSync(path.join(repoRoot, 'tsconfig.json'), TSCONFIG);
    fs.writeFileSync(
      path.join(repoRoot, 'src', 'model.ts'),
      'export interface Row { id: string; label: string }\n'
    );

    result = captureStub({
      repoRoot,
      serviceName: 'attribution-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: [
        // Clean: an addressable exported symbol. Its own statement names a
        // module that IS in the emitted tree.
        {
          kind: 'symbol',
          alias: 'Endpoint_clean_Response',
          symbol_name: 'Row',
          source_file: 'src/model.ts',
          anchor_origin: 'llm-symbol',
        },
        // Broken: a literal type text printed in another file's scope, whose
        // relative specifier resolves to nothing from the surface entry. This
        // is the real shape — `derive_capture_anchors` prefers the v1
        // inference RESULT as a literal anchor and pastes it verbatim.
        {
          kind: 'literal',
          alias: 'Endpoint_dangling_Response',
          type_text: "import('./nowhere').Missing",
          anchor_origin: 'deterministic-infer',
        },
      ],
    });
    byAlias = new Map(result.aliases.map((record) => [record.alias, record]));
  });

  after(() => {
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  it('captures both aliases', () => {
    assert.strictEqual(result.success, true, result.errors.join('; '));
    assert.strictEqual(byAlias.size, 2);
  });

  it('blames the alias whose own statement carries the dangling specifier', () => {
    const dangling = byAlias.get('Endpoint_dangling_Response')!;
    assert.strictEqual(dangling.self_check, 'decayed_internal');
    assert.match(String(dangling.self_check_detail), /nowhere/);
    assert.deepStrictEqual(dangling.dangling_specifiers, ['./nowhere']);
  });

  it('leaves the sibling alias clean, though they share the surface file', () => {
    const clean = byAlias.get('Endpoint_clean_Response')!;
    assert.strictEqual(
      clean.self_check,
      'ok',
      `a sibling's dangling specifier must not decay this alias: ${clean.self_check_detail}`
    );
    assert.strictEqual(clean.top_type_at_self_check, false);
    assert.strictEqual(clean.dangling_specifiers, undefined);
  });

  it('counts only the broken alias as unusable', () => {
    assert.strictEqual(result.fidelity.by_self_check.ok, 1);
    assert.strictEqual(result.fidelity.by_self_check.decayed_internal, 1);
  });
});
