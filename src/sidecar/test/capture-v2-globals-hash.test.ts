/**
 * J1 (carrick#1047): `carrick-manifest.json` records `globals_hash`, the hash
 * of the stub's global-scope files, `null` when it has none. A pair's side hash
 * folds every stub's value in, and the judge loads every stub that has one.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'ES2020', module: 'commonjs', strict: true, declaration: true, rootDir: './' },
  include: ['**/*.ts'],
});

const roots: string[] = [];

/** Capture `files` from a fresh repo; the manifest's `globals_hash`. */
function globalsHash(files: Record<string, string>): string | null | undefined {
  const outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-globals-hash-'));
  roots.push(outRoot);
  const repoRoot = path.join(outRoot, 'repo');
  fs.mkdirSync(path.join(repoRoot, 'src'), { recursive: true });
  fs.writeFileSync(path.join(repoRoot, 'tsconfig.json'), TSCONFIG);
  for (const [rel, text] of Object.entries(files)) fs.writeFileSync(path.join(repoRoot, rel), text);
  const result = captureStub({
    repoRoot,
    serviceName: 'globals-svc',
    outDir: path.join(outRoot, 'stub'),
    anchors: [
      {
        kind: 'symbol',
        alias: 'Endpoint_row_Response',
        symbol_name: 'Row',
        source_file: 'src/model.ts',
        anchor_origin: 'llm-symbol',
      },
    ],
  });
  assert.strictEqual(result.success, true, result.errors.join('; '));
  const manifest = JSON.parse(fs.readFileSync(path.join(result.stub_dir, 'carrick-manifest.json'), 'utf8'));
  return manifest.globals_hash;
}

const BASE: Record<string, string> = {
  'src/model.ts': 'export interface Row { id: string }\n',
  'src/handler.ts': 'import { Row } from "./model";\nexport function read(): Row {\n  return { id: "a" };\n}\n',
};
const AUGMENT = (member: string): Record<string, string> => ({
  ...BASE,
  'src/aug.ts': `export {};\ndeclare global { interface Window { ${member}: string } }\n`,
});

describe('capture v2: globals_hash (carrick#1047)', () => {
  after(() => {
    for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
  });

  it('is null for a stub with no global-scope file', () => {
    assert.strictEqual(globalsHash(BASE), null);
  });

  it('is a sha256 for a stub with an augmentation file, and moves when that file is edited', () => {
    const first = globalsHash(AUGMENT('one'));
    assert.match(String(first), /^[0-9a-f]{64}$/);
    assert.strictEqual(globalsHash(AUGMENT('one')), first);
    assert.notStrictEqual(globalsHash(AUGMENT('two')), first);
  });
});
