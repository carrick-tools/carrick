/**
 * carrick#3135: each alias's record carries `surface_hash`, the hash of what
 * the judge reads for it from its stub. A pull request run settles a pair it
 * reaches when both sides' hashes equal the ones main judged on, so the hash
 * must move with every input of that alias, and with nothing else: not with
 * the checkout's path, and not with another alias's declarations.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';

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

const roots: string[] = [];

/** Capture `files` from a fresh repo; each alias's surface hash. */
function hashes(
  files: Record<string, string>,
  anchors: Array<{ alias: string; symbol: string; file: string }> = [
    { alias: 'Endpoint_row_Response', symbol: 'Row', file: 'src/model.ts' },
    { alias: 'Endpoint_other_Response', symbol: 'Other', file: 'src/other.ts' },
  ]
): Map<string, string | undefined> {
  const outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-hash-'));
  roots.push(outRoot);
  const repoRoot = path.join(outRoot, 'repo');
  fs.mkdirSync(path.join(repoRoot, 'src'), { recursive: true });
  fs.writeFileSync(path.join(repoRoot, 'tsconfig.json'), TSCONFIG);
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(repoRoot, rel)), { recursive: true });
    fs.writeFileSync(path.join(repoRoot, rel), text);
  }
  const result = captureStub({
    repoRoot,
    serviceName: 'hash-svc',
    outDir: path.join(outRoot, 'stub'),
    anchors: anchors.map(({ alias, symbol, file }) => ({
      kind: 'symbol' as const,
      alias,
      symbol_name: symbol,
      source_file: file,
      anchor_origin: 'llm-symbol' as const,
    })),
  });
  assert.strictEqual(result.success, true, result.errors.join('; '));
  return new Map(result.aliases.map((record) => [record.alias, record.surface_hash]));
}

const BASE: Record<string, string> = {
  'src/model.ts': 'import { Label } from "./label";\nexport interface Row { id: string; label: Label }\n',
  'src/label.ts': 'export interface Label { text: string }\n',
  'src/other.ts': 'export interface Other { n: number }\n',
  'src/handler.ts':
    'import { Row } from "./model";\nexport function read(): Row {\n  return { id: "a", label: { text: "b" } };\n}\n',
};

describe('capture v2: surface hash (carrick#3135)', () => {
  after(() => {
    for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
  });

  const base = hashes(BASE);

  it('hashes every captured alias, the same tree alike at another path', () => {
    assert.match(String(base.get('Endpoint_row_Response')), /^[0-9a-f]{64}$/);
    assert.match(String(base.get('Endpoint_other_Response')), /^[0-9a-f]{64}$/);
    assert.notStrictEqual(base.get('Endpoint_row_Response'), base.get('Endpoint_other_Response'));
    assert.deepStrictEqual(hashes(BASE), base);
  });

  it('keeps both hashes when a function body changes and no declaration does', () => {
    const edited = hashes({
      ...BASE,
      'src/handler.ts':
        'import { Row } from "./model";\nexport function read(): Row {\n  return { id: "z", label: { text: "y" } };\n}\n',
    });
    assert.deepStrictEqual(edited, base);
  });

  it('moves only the alias whose closure holds a changed declaration', () => {
    const edited = hashes({
      ...BASE,
      'src/label.ts': 'export interface Label { text: string; lang: string }\n',
    });
    assert.notStrictEqual(edited.get('Endpoint_row_Response'), base.get('Endpoint_row_Response'));
    assert.strictEqual(edited.get('Endpoint_other_Response'), base.get('Endpoint_other_Response'));
  });

  it('moves every alias when a global augmentation in the tree changes', () => {
    const augmented = hashes({
      ...BASE,
      'src/globals.ts': 'export {};\ndeclare global {\n  interface Carried { a: string }\n}\n',
    });
    const changed = hashes({
      ...BASE,
      'src/globals.ts': 'export {};\ndeclare global {\n  interface Carried { a: number }\n}\n',
    });
    for (const alias of ['Endpoint_row_Response', 'Endpoint_other_Response']) {
      assert.notStrictEqual(augmented.get(alias), changed.get(alias), alias);
    }
  });
});

/**
 * Aliases anchored at barrels: the hash must move exactly when reach marks
 * the barrel (`type-reach.test.ts`, "reach marks the barrels").
 */
const BARRELS: Record<string, string> = {
  'src/kinds/status.ts':
    "export type Status = 'open' | 'closed';\nexport interface Ticket { status: Status }\nfunction tally(): number {\n  return 1;\n}\nexport const label = 'kind';\n",
  'src/kinds/index.ts': "export * from './status';\n",
  'src/top/index.ts': "export * from '../kinds';\n",
  'src/named.ts': "export { Ticket } from './kinds/status';\n",
};

const BARREL_ANCHORS = [
  { alias: 'Endpoint_star_Response', symbol: 'Ticket', file: 'src/kinds/index.ts' },
  { alias: 'Endpoint_chain_Response', symbol: 'Ticket', file: 'src/top/index.ts' },
  { alias: 'Endpoint_named_Response', symbol: 'Ticket', file: 'src/named.ts' },
];

describe('capture v2: surface hash of aliases anchored at barrels', () => {
  after(() => {
    for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
  });

  const base = hashes(BARRELS, BARREL_ANCHORS);
  const statusAt = (edited: (text: string) => string) =>
    hashes({ ...BARRELS, 'src/kinds/status.ts': edited(BARRELS['src/kinds/status.ts']!) }, BARREL_ANCHORS);

  it('captures every barrel alias', () => {
    for (const { alias } of BARREL_ANCHORS) assert.match(String(base.get(alias)), /^[0-9a-f]{64}$/, alias);
  });

  it('moves every barrel alias when a union member is added behind it', () => {
    const edited = statusAt((t) => t.replace("'open' | 'closed'", "'open' | 'closed' | 'held'"));
    for (const { alias } of BARREL_ANCHORS) assert.notStrictEqual(edited.get(alias), base.get(alias), alias);
  });

  it('keeps every barrel alias when only a non-exported function body changes', () => {
    assert.deepStrictEqual(statusAt((t) => t.replace('return 1;', 'return 2;')), base);
  });
});
