/**
 * An `infer` answer is kept per service and served again while the program it
 * was read in is the same program (carrick#2843).
 *
 * Pinned over stdio, each step in a fresh process as a scan runs it:
 * - the second process asked the same request of the same tree is served the
 *   kept answer, identical to the first, and holds the same program after;
 * - a declaration file the program reads that changed is read again;
 * - a file that now resolves in place of another is read again;
 * - a file outside the program that changed does not cost the answer;
 * - a different extraction config is read again;
 * - with the cache off nothing is kept.
 */

import { describe, it, beforeEach, afterEach } from 'node:test';
import * as assert from 'node:assert';
import { spawn } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as readline from 'node:readline';
import type { InferRequestItem } from '../src/types.js';
import { INFER_CACHE_ENV } from '../src/infer-cache.js';
import { SIDECAR_PATH } from './helpers.js';

interface Frame {
  request_id: string;
  status: string;
  inferred_types?: Array<{ alias: string; type_string: string }>;
  files?: string[];
  errors?: string[];
}

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
  include: ['src/**/*.ts'],
});

const ORDERS = `import { makeShape } from '../types/shape';
export function label(order: { id: string }) {
  return { title: makeShape().title, id: order.id };
}
`;

const SHAPE_DTS = `export declare function makeShape(): { title: string };
`;

/** Answers of one fresh process, and whether it said it served a kept answer. */
interface Run {
  frames: Frame[];
  served: boolean;
}

/** Send `requests` to a fresh sidecar with `env`, one at a time. */
async function run(requests: Array<Record<string, unknown>>, env: NodeJS.ProcessEnv): Promise<Run> {
  const child = spawn('node', [SIDECAR_PATH], { stdio: ['pipe', 'pipe', 'pipe'], env });
  let stderr = '';
  child.stderr.on('data', (chunk: Buffer) => {
    stderr += chunk.toString();
  });
  const lines = readline.createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  const frames: Frame[] = [];
  try {
    for (const request of requests) {
      child.stdin.write(JSON.stringify(request) + '\n');
      for (;;) {
        const next = await lines.next();
        assert.ok(!next.done, `the sidecar closed before answering ${String(request.request_id)}`);
        if (!next.value.trim()) continue;
        const frame = JSON.parse(next.value) as Frame;
        if (frame.request_id !== request.request_id || frame.status === 'progress') continue;
        frames.push(frame);
        break;
      }
    }
  } finally {
    child.kill();
  }
  return { frames, served: stderr.includes('from the kept answer') };
}

describe('a kept infer answer (carrick#2843)', () => {
  let root: string;
  let cache: string;
  let orders: string;
  let env: NodeJS.ProcessEnv;

  const item = (): InferRequestItem => ({
    file_path: orders,
    line_number: 2,
    infer_kind: 'signature_return',
    alias: 'label_return',
  });

  /** init, infer, then the program's files, as a scan's process is asked. */
  const scan = (extra: Record<string, unknown> = {}, withEnv = env): Promise<Run> =>
    run(
      [
        { action: 'init', request_id: 'init', repo_root: root },
        { action: 'infer', request_id: 'infer', requests: [item()], ...extra },
        { action: 'list_program_files', request_id: 'files' },
      ],
      withEnv
    );

  const typeOf = (r: Run): string | undefined => r.frames[1].inferred_types?.[0]?.type_string;

  beforeEach(() => {
    root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-infer-cache-')));
    cache = path.join(root, '.cache');
    fs.mkdirSync(path.join(root, 'src'));
    fs.mkdirSync(path.join(root, 'types'));
    fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
    orders = path.join(root, 'src', 'orders.ts');
    fs.writeFileSync(orders, ORDERS);
    fs.writeFileSync(path.join(root, 'types', 'shape.d.ts'), SHAPE_DTS);
    env = { ...process.env, [INFER_CACHE_ENV]: cache };
  });

  afterEach(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('serves the same answer to the same request of the same tree, and leaves the same program', async () => {
    const first = await scan();
    assert.strictEqual(first.frames[1].status, 'success', JSON.stringify(first.frames[1].errors));
    assert.strictEqual(first.served, false);
    assert.match(typeOf(first) ?? '', /title: string/);

    const second = await scan();
    assert.strictEqual(second.served, true, 'the second process inferred again');
    assert.deepStrictEqual(second.frames[1].inferred_types, first.frames[1].inferred_types);
    assert.strictEqual(second.frames[1].status, first.frames[1].status);
    assert.deepStrictEqual(second.frames[2].files, first.frames[2].files);
  });

  it('reads again when a declaration the program reads changed', async () => {
    await scan();
    fs.writeFileSync(path.join(root, 'types', 'shape.d.ts'), 'export declare function makeShape(): { title: number };\n');
    const after = await scan();
    assert.strictEqual(after.served, false, 'a changed declaration was served from the kept answer');
    assert.match(typeOf(after) ?? '', /title: number/);
    // And the answer read again is kept in turn.
    assert.strictEqual((await scan()).served, true);
  });

  it('reads again when a file now resolves in place of the one the program read', async () => {
    await scan();
    // A `.ts` beside the `.d.ts` is what the import now resolves to.
    fs.writeFileSync(path.join(root, 'types', 'shape.ts'), 'export function makeShape() { return { title: true }; }\n');
    const after = await scan();
    assert.strictEqual(after.served, false, 'a new resolution was served from the kept answer');
    assert.match(typeOf(after) ?? '', /title: boolean/);
  });

  it('serves the kept answer when only a file outside the program changed', async () => {
    const first = await scan();
    fs.writeFileSync(path.join(root, 'README.md'), 'changed\n');
    fs.writeFileSync(path.join(root, 'notes.txt'), 'not a source file\n');
    const after = await scan();
    assert.strictEqual(after.served, true);
    assert.deepStrictEqual(after.frames[1].inferred_types, first.frames[1].inferred_types);
  });

  it('reads again when the extraction config differs', async () => {
    await scan();
    const after = await scan({
      extraction_config: { rules: [] },
    });
    assert.strictEqual(after.served, false, 'a different extraction config was served the kept answer');
  });

  it('keeps nothing when the cache is off', async () => {
    const off = { ...process.env };
    delete off[INFER_CACHE_ENV];
    await scan({}, off);
    await scan({}, off);
    assert.strictEqual(fs.existsSync(cache), false);
  });
});
