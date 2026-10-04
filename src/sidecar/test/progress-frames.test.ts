/**
 * Long requests say they are alive (carrick#1914).
 *
 * The scanner's deadline for a request is how long the sidecar may stay
 * silent about it. `infer` can hold thousands of requests in one batch, so it
 * writes a `progress` frame as it finishes them; `bundle` and
 * `verify_library_claims` write one when the program they read is ready,
 * which is the part of their work whose size the request does not bound.
 *
 * Two layers are pinned:
 * - the inferrer tells its caller as it gets past each request, between
 *   requests and not at the end, whatever the request answered;
 * - over stdio, those requests write `progress` frames before their terminal
 *   frame, under their own request id, and never after it.
 *
 * The repo is written to the OS temp dir. Nothing here is timed: pacing is
 * tested with a clock in `progress.test.ts`, and that a frame written by a
 * blocked handler leaves the process at once is tested from the reader's side
 * in the scanner's `sidecar_operation_timeout_test.rs`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as readline from 'node:readline';
import { Project } from 'ts-morph';
import { TypeInferrer } from '../src/type-inferrer.js';
import type { InferRequestItem } from '../src/types.js';
import { SIDECAR_PATH } from './helpers.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
  include: ['src/**/*.ts'],
});

const WORK = `export interface Receipt {
  id: string;
  total: number;
}
export function one() {
  return { id: 'a', total: 1 };
}
export function two() {
  return [one()];
}
export function three() {
  return two().length;
}
`;

const lineOf = (source: string, text: string): number => {
  const index = source.split('\n').findIndex((line) => line.includes(text));
  assert.ok(index >= 0, `fixture has no line containing ${text}`);
  return index + 1;
};

interface Frame {
  request_id: string;
  status: string;
  phase?: string;
  message?: string;
  inferred_types?: Array<{ alias: string; type_string: string }>;
  errors?: string[];
}

/** A sidecar whose every stdout frame is kept, progress frames included. */
class RawSidecar {
  private readonly child: ChildProcessWithoutNullStreams;
  private readonly frames: Frame[] = [];
  private waiting: (() => void) | null = null;

  constructor() {
    this.child = spawn('node', [SIDECAR_PATH], { stdio: ['pipe', 'pipe', 'pipe'] });
    this.child.stderr.resume();
    readline.createInterface({ input: this.child.stdout }).on('line', (line) => {
      if (!line.trim()) return;
      this.frames.push(JSON.parse(line) as Frame);
      this.waiting?.();
    });
  }

  /** Send one request and return every frame up to and including its terminal one. */
  async exchange(request: Record<string, unknown>): Promise<Frame[]> {
    const from = this.frames.length;
    this.child.stdin.write(JSON.stringify(request) + '\n');
    const terminal = () =>
      this.frames.slice(from).some((f) => f.request_id === request.request_id && f.status !== 'progress');
    while (!terminal()) {
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`no terminal frame for ${request.request_id}`)), 60_000);
        this.waiting = () => {
          clearTimeout(timer);
          resolve();
        };
      });
    }
    this.waiting = null;
    return this.frames.slice(from);
  }

  /** Every frame the process has written. */
  all(): Frame[] {
    return [...this.frames];
  }

  stop(): void {
    this.child.kill();
  }
}

describe('long requests report progress (carrick#1914)', () => {
  let root: string;
  let workPath: string;
  let requests: InferRequestItem[];

  before(() => {
    root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-progress-')));
    fs.mkdirSync(path.join(root, 'src'));
    fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
    workPath = path.join(root, 'src', 'work.ts');
    fs.writeFileSync(workPath, WORK);
    requests = ['one', 'two', 'three'].map((name) => ({
      file_path: workPath,
      line_number: lineOf(WORK, `export function ${name}()`),
      infer_kind: 'function_return' as const,
      alias: name,
    }));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  describe('the inferrer', () => {
    const inferrer = () =>
      new TypeInferrer({
        project: new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') }),
        repoRoot: root,
      });

    it('tells its caller once per request, whatever the request answered', () => {
      const batch: InferRequestItem[] = [
        requests[0],
        // Skipped: plain JavaScript is not inferred.
        { ...requests[0], file_path: path.join(root, 'src', 'plain.js'), alias: 'skipped' },
        // Refused: an expression with neither a span nor its text.
        { file_path: workPath, line_number: 1, infer_kind: 'expression', alias: 'refused' },
        // Not found: no function on that line.
        { ...requests[0], line_number: 9999, alias: 'nowhere' },
        requests[1],
      ];
      let told = 0;
      const result = inferrer().infer(batch, undefined, () => {
        told += 1;
      });
      assert.strictEqual(told, batch.length);
      assert.deepStrictEqual(
        (result.inferred_types ?? []).map((t) => t.alias),
        ['one', 'two']
      );
    });

    it('tells it between requests, not once the batch is over', () => {
      // The third request names a file that does not exist until the caller
      // is told the first is done. It can only be answered if that telling
      // came before the batch reached it.
      const late = { ...requests[2], file_path: path.join(root, 'src', 'not-yet.ts') };
      const batch = [requests[0], requests[1], late];
      let told = 0;
      const result = inferrer().infer(batch, undefined, () => {
        told += 1;
        if (told === 1) late.file_path = workPath;
      });
      assert.deepStrictEqual(
        (result.inferred_types ?? []).map((t) => t.alias),
        ['one', 'two', 'three'],
        JSON.stringify(result.errors)
      );
    });

    it('asks for no callback: a batch with none answers as before', () => {
      const result = inferrer().infer(requests);
      assert.deepStrictEqual(
        (result.inferred_types ?? []).map((t) => t.alias),
        ['one', 'two', 'three']
      );
    });
  });

  describe('over stdio', () => {
    let sidecar: RawSidecar;

    before(async () => {
      sidecar = new RawSidecar();
      const [ready] = await sidecar.exchange({ action: 'init', request_id: 'progress-init', repo_root: root });
      assert.strictEqual(ready.status, 'ready');
    });

    after(() => {
      sidecar.stop();
    });

    const progressOf = (frames: Frame[]) => frames.filter((f) => f.status === 'progress');

    it('infer reports how far through the batch it is, then answers', async () => {
      const frames = await sidecar.exchange({ action: 'infer', request_id: 'progress-infer', requests });
      const terminal = frames[frames.length - 1];
      assert.strictEqual(terminal.status, 'success', JSON.stringify(terminal.errors));
      assert.deepStrictEqual(
        (terminal.inferred_types ?? []).map((t) => t.alias),
        ['one', 'two', 'three']
      );

      const progress = progressOf(frames);
      assert.strictEqual(progress.length, frames.length - 1, 'only progress frames precede the answer');
      assert.ok(progress.length >= 1, 'a batch reports the first request it finishes');
      // The first report is not paced, so it is always the first request.
      assert.strictEqual(progress[0].message, '1 of 3');
      let last = 0;
      for (const frame of progress) {
        assert.strictEqual(frame.request_id, 'progress-infer');
        assert.strictEqual(frame.phase, 'infer');
        const [, done, total] = /^(\d+) of (\d+)$/.exec(frame.message ?? '') ?? [];
        assert.strictEqual(Number(total), 3, `unreadable progress message: ${frame.message}`);
        assert.ok(Number(done) > last && Number(done) <= 3, `count went ${last} -> ${done}`);
        last = Number(done);
      }
    });

    it('bundle reports that its program is ready, then answers', async () => {
      const frames = await sidecar.exchange({
        action: 'bundle',
        request_id: 'progress-bundle',
        symbols: [{ symbol_name: 'Receipt', source_file: 'src/work.ts', alias: 'Receipt_Alias' }],
      });
      assert.strictEqual(frames[frames.length - 1].status, 'success', JSON.stringify(frames));
      assert.deepStrictEqual(
        progressOf(frames).map((f) => [f.request_id, f.phase, f.message]),
        [['progress-bundle', 'bundle', 'program ready']]
      );
      assert.strictEqual(frames.length, 2);
    });

    it('verify_library_claims reports that its program is ready, then answers', async () => {
      const frames = await sidecar.exchange({
        action: 'verify_library_claims',
        request_id: 'progress-verify',
        from_dir: root,
        checks: [],
      });
      assert.strictEqual(frames[frames.length - 1].status, 'success', JSON.stringify(frames));
      assert.deepStrictEqual(
        progressOf(frames).map((f) => [f.request_id, f.phase, f.message]),
        [['progress-verify', 'verify_library_claims', 'program ready']]
      );
      assert.strictEqual(frames.length, 2);
    });

    it('a request that fails before any work writes its error and no progress', async () => {
      const frames = await sidecar.exchange({
        action: 'infer',
        request_id: 'progress-refused',
        requests: 'not a list',
      });
      assert.deepStrictEqual(
        frames.map((f) => [f.request_id, f.status]),
        [['progress-refused', 'error']]
      );
    });

    it('never writes a progress frame after the answer it belongs to', () => {
      const answered = new Set<string>();
      for (const frame of sidecar.all()) {
        if (frame.status === 'progress') {
          assert.ok(!answered.has(frame.request_id), `late progress frame for ${frame.request_id}`);
        } else {
          answered.add(frame.request_id);
        }
      }
    });
  });
});
