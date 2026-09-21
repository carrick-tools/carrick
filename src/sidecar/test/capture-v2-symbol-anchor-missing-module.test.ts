/**
 * Regression for carrick#1397: a SYMBOL anchor whose emitted declaration
 * imports a module that does not resolve was discarded whole.
 *
 * carrick#1377 kept a type whose printed answer named something undeclared,
 * but only on the two PRINT paths. A symbol anchor prints nothing: its surface
 * line is `import('./m').RenderRequest` and the shape lives in the `.d.ts` the
 * compiler emitted for `m`. When that emitted file carries an import the
 * checkout cannot resolve, the stub reports the module missing, the capture
 * records it as a dangling internal specifier, and the scanner's publish gate
 * refuses the alias — so a request type with twenty resolved members published
 * nothing because of one member.
 *
 * The emitted declaration is repaired instead: the import that did not resolve
 * is dropped and every type it bound reads `unknown` at its own position. That
 * is file-granular, so every alias whose closure reaches the file benefits at
 * once, and it is the same statement the source program already makes — the
 * names were TypeScript's unresolved-reference placeholder there too.
 *
 * The pair is still not judged compatible: the deep-decay rule reads the
 * `unknown` and abstains, naming the member. What this buys is the type
 * reaching the index.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord } from '../src/capture/api.js';

const CLIENT = 'src/client.ts';

const FILES: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'ESNext',
      moduleResolution: 'Bundler',
      strict: true,
      skipLibCheck: true,
      declaration: true,
      rootDir: 'src',
    },
    include: ['src'],
  }),
  [CLIENT]: [
    // The package is not installed on this checkout, so `Evidence` resolves to
    // nothing and every member typed with it is a placeholder.
    "import type { Evidence } from 'evidence-sdk';",
    '',
    'export interface LineItem {',
    '  id: string;',
    '  amount: number;',
    '  evidence: Evidence;',
    '}',
    '',
    'export interface RenderRequest {',
    '  documentId: string;',
    '  items: LineItem[];',
    '}',
    '',
  ].join('\n'),
};

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1397: a symbol anchor survives a missing module in its declaration', () => {
  let root: string;
  let records: Map<string, CaptureAliasRecord>;
  let emitted: string;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1397-'));
    const repoRoot = path.join(root, 'repo');
    for (const [rel, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(repoRoot, rel)), { recursive: true });
      fs.writeFileSync(path.join(repoRoot, rel), text);
    }
    const result = captureStub({
      repoRoot,
      serviceName: 'symbol-missing-module',
      outDir: path.join(root, 'stub'),
      anchors: [
        {
          kind: 'symbol',
          alias: 'Endpoint_render_Request',
          symbol_name: 'RenderRequest',
          source_file: CLIENT,
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    records = new Map(result.aliases.map((record) => [record.alias, record]));
    const clientDts = path.join(root, 'stub', 'types', 'client.d.ts');
    emitted = fs.readFileSync(clientDts, 'utf-8');
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('keeps the resolved members and writes the unresolvable one `unknown`', () => {
    assert.match(emitted, /documentId: string/);
    assert.match(emitted, /amount: number/);
    assert.match(
      emitted,
      /evidence: unknown/,
      `the member typed by the missing module must read 'unknown', got: ${collapse(emitted)}`
    );
    assert.doesNotMatch(
      emitted,
      /evidence: Evidence/,
      'a name the missing module bound must not survive into the emitted tree'
    );
    assert.doesNotMatch(
      emitted,
      /from ['"]evidence-sdk['"]/,
      'the import that did not resolve must be gone, or the stub still reports the module missing'
    );
  });

  it('publishes the alias: nothing in its closure dangles any more', () => {
    const record = records.get('Endpoint_render_Request');
    assert.ok(record, 'the alias must have a record');
    assert.deepStrictEqual(
      record.dangling_specifiers ?? [],
      [],
      `the repaired declaration names no missing module: ${JSON.stringify(record)}`
    );
    assert.deepStrictEqual(record.undeclared_names ?? [], []);
    assert.strictEqual(record.top_type_at_self_check, false);
    assert.strictEqual(
      record.serialization,
      'emitted',
      'the members that DID resolve keep the emitted declaration behind them'
    );
  });

  it('labels the substituted member unresolved_import, not declared', () => {
    const record = records.get('Endpoint_render_Request');
    assert.ok(record, 'the alias must have a record');
    const finding = (record.any_provenance ?? []).find((entry) =>
      entry.path.endsWith('evidence')
    );
    assert.ok(
      finding,
      `the member must be named by its own path, got: ${JSON.stringify(record.any_provenance)}`
    );
    assert.strictEqual(finding.kind, 'unknown');
    assert.strictEqual(
      finding.reason,
      'unresolved_import',
      "a reader told 'declared' stops looking where the fix is: install the module"
    );
  });
});
