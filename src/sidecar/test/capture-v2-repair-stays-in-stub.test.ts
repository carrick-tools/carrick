/**
 * Regression for carrick#1742: the capture's dangling-import repair rewrote
 * the scanned repo's own source files.
 *
 * The self-check links the repo's `node_modules` into the stub so that the
 * emitted declarations resolve their dependencies. In a workspace, one of
 * those dependencies is a sibling package linked into `node_modules`, and its
 * `types` can be the package's TypeScript SOURCE. The self-check program then
 * loads the repo's own files. When one of them imports a module the checkout
 * does not have (a generated client that was never generated), the compiler
 * reports it missing in THAT file, and the repair (carrick#1397) dropped the
 * import and wrote `unknown` over its names, on disk, in the user's tree.
 *
 * The repair exists for the declarations the capture emitted into its own
 * stub. A file outside the stub is the user's, and a scan reads it, never
 * writes it.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';

const CLIENT = 'src/client.ts';
const SHARED_SOURCE = 'packages/shared/src/index.ts';

const FILES: Record<string, string> = {
  'package.json': JSON.stringify({ name: 'repair-root', private: true }),
  'tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'ESNext',
      moduleResolution: 'Bundler',
      strict: true,
      skipLibCheck: true,
      declaration: true,
    },
    include: ['src'],
  }),
  [CLIENT]: [
    "import type { Shared } from '@ws/shared';",
    '',
    'export interface RenderRequest {',
    '  documentId: string;',
    '  shared: Shared;',
    '}',
    '',
  ].join('\n'),
  // A workspace package whose declarations ARE its TypeScript source.
  'packages/shared/package.json': JSON.stringify({
    name: '@ws/shared',
    version: '1.0.0',
    types: './src/index.ts',
    exports: { '.': { types: './src/index.ts', default: './src/index.ts' } },
  }),
  // It imports a module this checkout does not have.
  [SHARED_SOURCE]: [
    "import type { Row } from 'generated-db-client';",
    '',
    'export interface Shared {',
    '  id: string;',
    '  row: Row;',
    '}',
    '',
  ].join('\n'),
};

/** Every file and link under `dir`, with a content hash (or the link target). */
function snapshot(dir: string): Record<string, string> {
  const out: Record<string, string> = {};
  const walk = (current: string) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const abs = path.join(current, entry.name);
      const rel = path.relative(dir, abs);
      if (entry.isSymbolicLink()) {
        out[rel] = `link:${fs.readlinkSync(abs)}`;
      } else if (entry.isDirectory()) {
        out[rel + '/'] = 'dir';
        walk(abs);
      } else {
        out[rel] = crypto.createHash('sha256').update(fs.readFileSync(abs)).digest('hex');
      }
    }
  };
  walk(dir);
  return out;
}

describe('carrick#1742: the capture never writes to the scanned repo', () => {
  let root: string;
  let repoRoot: string;
  let before_: Record<string, string>;
  let sharedBefore: string;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1742-'));
    repoRoot = path.join(root, 'repo');
    for (const [rel, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(repoRoot, rel)), { recursive: true });
      fs.writeFileSync(path.join(repoRoot, rel), text);
    }
    // The workspace link a package manager makes.
    fs.mkdirSync(path.join(repoRoot, 'node_modules', '@ws'), { recursive: true });
    fs.symlinkSync('../../packages/shared', path.join(repoRoot, 'node_modules', '@ws', 'shared'), 'dir');
    before_ = snapshot(repoRoot);
    sharedBefore = fs.readFileSync(path.join(repoRoot, SHARED_SOURCE), 'utf-8');

    const result = captureStub({
      repoRoot,
      serviceName: 'repair-stays-in-stub',
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
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('leaves the workspace package source that imports a missing module byte-identical', () => {
    const after_ = fs.readFileSync(path.join(repoRoot, SHARED_SOURCE), 'utf-8');
    assert.strictEqual(
      after_,
      sharedBefore,
      'the capture rewrote a source file of the scanned repo'
    );
  });

  it('adds, removes and changes nothing anywhere in the scanned repo', () => {
    assert.deepStrictEqual(snapshot(repoRoot), before_);
  });
});
