/**
 * Regression for carrick#1377: one nested member that resolves to nothing
 * discarded the WHOLE type.
 *
 * A request body typed `{ documentId: string; items: { id: string; evidence:
 * Evidence }[] }`, where `Evidence` comes from a package the scanned checkout
 * does not have, printed an answer naming `Evidence` — an identifier nothing
 * declares. carrick#1165 records that as `undeclared_names`, and the scanner's
 * publish gate refuses the alias entirely, so the request type read null and
 * the documentId and the rest of the items went with it.
 *
 * A member that did not resolve is now written `unknown` at its own position
 * and everything around it survives. The substitution is recorded as an
 * unresolved placeholder, so the self-check labels it `unresolved_import` —
 * "install or generate the missing module" — rather than `declared`, which
 * would tell a reader the author wrote `unknown` there on purpose.
 *
 * `undeclared_names` is NOT retired by this. It stays as what it always was —
 * what the PRINTED answer names and nothing declares — and is now empty
 * because the print no longer names it. Anything substitution cannot reach
 * still fills it and still refuses publication.
 *
 * The pair is still not judged compatible: the capture deep-decay pre-gate
 * reads the `unknown` and abstains, naming the member. That is the soundness
 * rule (a partially-unresolved type would let an arbitrary shape read
 * compatible) and is deliberately untouched. What changed is that the type
 * reaches the index at all, with the one member it could not see marked.
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
      rootDir: 'src',
    },
    include: ['src'],
  }),
  [CLIENT]: [
    // The package is not installed on this checkout, so `Evidence` resolves
    // to nothing and every member typed with it is a placeholder.
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
    'declare function postJson(url: string, body: string): Promise<Uint8Array>;',
    'declare function loadEvidence(id: string): Promise<Evidence>;',
    '',
    'export async function render(payload: RenderRequest): Promise<Uint8Array> {',
    '  return postJson("https://pdf.internal/pdf", JSON.stringify(payload));',
    '}',
    '',
    // An anonymous shape the node builder prints by REUSING the source
    // annotation, which is the one way its print can name something the
    // program does not declare.
    'export async function describe(id: string) {',
    '  const evidence: Evidence = await loadEvidence(id);',
    '  const answer = { documentId: id, evidence };',
    '  return answer;',
    '}',
    '',
  ].join('\n'),
};

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1377: one unresolvable member does not discard the type', () => {
  let root: string;
  let records: Map<string, CaptureAliasRecord>;
  let surface: string;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1377-'));
    const repoRoot = path.join(root, 'repo');
    for (const [rel, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(repoRoot, rel)), { recursive: true });
      fs.writeFileSync(path.join(repoRoot, rel), text);
    }
    const result = captureStub({
      repoRoot,
      serviceName: 'nested-unresolved',
      outDir: path.join(root, 'stub'),
      anchors: [
        {
          // The shape a consumer request takes: v1 printed the body's members
          // and `derive_capture_anchors` hands that text back as a literal.
          kind: 'literal',
          alias: 'Endpoint_pdf_Request',
          type_text:
            '{ documentId: string; items: { id: string; amount: number; evidence: Evidence; }[]; }',
          anchor_origin: 'deterministic-infer',
          source_file: CLIENT,
        },
        {
          // The same loss through the node builder rather than literal text.
          kind: 'infer',
          alias: 'Endpoint_describe_Response',
          source_file: CLIENT,
          anchor_origin: 'deterministic-infer',
          line_number:
            FILES[CLIENT].split('\n').findIndex((l) => l.includes('return answer;')) + 1,
          expression_text: 'answer',
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    records = new Map(result.aliases.map((record) => [record.alias, record]));
    surface = fs.readFileSync(
      path.join(root, 'stub', 'types', 'surface.d.ts'),
      'utf-8'
    );
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('keeps the resolved members and writes the unresolvable one `unknown`', () => {
    assert.match(surface, /documentId: string/);
    assert.match(surface, /amount: number/);
    assert.match(
      surface,
      /evidence: unknown/,
      `the member that did not resolve must read 'unknown', got: ${collapse(surface)}`
    );
    assert.doesNotMatch(
      surface,
      /evidence: Evidence/,
      'a name nothing declares must not survive into the surface'
    );
  });

  it('publishes the alias: the printed answer no longer names anything undeclared', () => {
    const record = records.get('Endpoint_pdf_Request');
    assert.ok(record, 'the alias must have a record');
    assert.deepStrictEqual(
      record.undeclared_names ?? [],
      [],
      'undeclared_names describes the PRINTED answer, which no longer names it'
    );
    assert.strictEqual(record.top_type_at_self_check, false);
  });

  it('labels the substituted member unresolved_import, not declared', () => {
    const record = records.get('Endpoint_pdf_Request');
    assert.ok(record, 'the alias must have a record');
    const finding = (record.any_provenance ?? []).find(
      (entry) => entry.path === 'items<0>.evidence'
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

  it('does the same for an answer the node builder printed', () => {
    // The builder reuses a source annotation as written when the annotation's
    // import did not resolve, which is the one way its print names something
    // the program does not declare.
    const record = records.get('Endpoint_describe_Response');
    assert.ok(record, 'the alias must have a record');
    assert.deepStrictEqual(record.undeclared_names ?? [], []);
    assert.match(surface, /evidence: unknown/);
    const finding = (record.any_provenance ?? []).find((entry) =>
      entry.path.endsWith('evidence')
    );
    assert.ok(
      finding,
      `the member must be named by its own path, got: ${JSON.stringify(record.any_provenance)}`
    );
    assert.strictEqual(finding.reason, 'unresolved_import');
  });
});
