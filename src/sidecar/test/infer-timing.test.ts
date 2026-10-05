/**
 * An `infer` batch says which of its requests were slow (carrick#1985).
 *
 * A pass of thousands of requests can spend most of its time in a handful of
 * them, and its answer used to say nothing about which. Every request is now
 * timed, and the answer names the slowest few beside the types: for each, the
 * request (alias, file, line, kind), its wall time, how long the type it
 * printed is, and whether it was the first request this process was asked of
 * its file.
 *
 * Three layers are pinned:
 * - the summary of a batch's timings: which requests it names, in what order,
 *   and what it adds up;
 * - the inferrer: every request is timed whatever it answered, and the
 *   timing sits beside the answers, never in them;
 * - over stdio: the terminal frame carries the timing, for a request one
 *   project answers and for one two projects answer.
 *
 * Nothing here reads a clock: which request is slowest is tested on timings
 * the test writes, and the inferrer's own are only checked for being there.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as readline from 'node:readline';
import { Project } from 'ts-morph';
import { SLOWEST_SLOTS, inferTiming, mergeInferTimings } from '../src/infer-timing.js';
import { TypeInferrer } from '../src/type-inferrer.js';
import type { InferRequestItem, InferSlotTiming, InferTiming } from '../src/types.js';
import { SIDECAR_PATH } from './helpers.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
  include: ['src/**/*.ts'],
});

const ORDERS = `export function total(prices: number[], discount) {
  return prices.reduce((sum, price) => sum + price, 0);
}
export function label(order: { id: string }) {
  return { title: 'Order ' + order.id, lines: [1, 2, 3] };
}
`;

const STOCK = `export function inStock() {
  return true;
}
`;

const lineOf = (source: string, text: string): number => {
  const index = source.split('\n').findIndex((line) => line.includes(text));
  assert.ok(index >= 0, `fixture has no line containing ${text}`);
  return index + 1;
};

/** A timing the test writes: request `alias` took `ms` and printed `printed` characters. */
const slot = (alias: string, ms: number, printed = 0, first = false): InferSlotTiming => ({
  alias,
  file_path: `/repo/src/${alias}.ts`,
  line_number: 1,
  infer_kind: 'signature_return',
  ms,
  printed_length: printed,
  first_in_file: first,
});

const aliases = (slots: InferSlotTiming[]): Array<string | undefined> => slots.map((s) => s.alias);

describe('an infer batch names its slowest requests (carrick#1985)', () => {
  describe('the summary of a batch', () => {
    it('names the slowest requests, slowest first, and counts every one', () => {
      const timing = inferTiming([slot('quick', 1), slot('slowest', 900), slot('slow', 40), slot('quicker', 0.5)]);
      assert.deepStrictEqual(aliases(timing.slowest), ['slowest', 'slow', 'quick', 'quicker']);
      assert.strictEqual(timing.slots, 4);
      assert.strictEqual(timing.slots_ms, 941.5);
    });

    it('names no more than its cap, and still adds up the rest', () => {
      const many = Array.from({ length: SLOWEST_SLOTS + 10 }, (_, i) => slot(`s${i}`, i + 1));
      const timing = inferTiming(many);
      assert.strictEqual(timing.slowest.length, SLOWEST_SLOTS);
      assert.strictEqual(timing.slowest[0].alias, `s${SLOWEST_SLOTS + 9}`);
      assert.strictEqual(timing.slowest[SLOWEST_SLOTS - 1].alias, 's10');
      assert.strictEqual(timing.slots, SLOWEST_SLOTS + 10);
      const n = SLOWEST_SLOTS + 10;
      assert.strictEqual(timing.slots_ms, (n * (n + 1)) / 2);
    });

    it('keeps requests that took the same time in the order they were asked', () => {
      const timing = inferTiming([slot('a', 5), slot('b', 7), slot('c', 5), slot('d', 5)]);
      assert.deepStrictEqual(aliases(timing.slowest), ['b', 'a', 'c', 'd']);
    });

    it('adds up the requests that were the first asked of their file apart from the rest', () => {
      const timing = inferTiming([
        slot('first', 300, 0, true),
        slot('second', 2),
        slot('other-file', 50, 0, true),
        slot('third', 1),
      ]);
      assert.strictEqual(timing.first_in_file_slots, 2);
      assert.strictEqual(timing.first_in_file_ms, 350);
      assert.strictEqual(timing.slots_ms, 353);
    });

    it('names the request that printed the longest type, slow or not', () => {
      const many = [
        ...Array.from({ length: SLOWEST_SLOTS }, (_, i) => slot(`slow${i}`, 100 + i, 10)),
        slot('long-and-quick', 0.2, 5_000),
        slot('short', 0.1, 12),
      ];
      const timing = inferTiming(many);
      assert.ok(!aliases(timing.slowest).includes('long-and-quick'));
      assert.strictEqual(timing.longest_printed?.alias, 'long-and-quick');
      assert.strictEqual(timing.longest_printed?.printed_length, 5_000);
    });

    it('names no longest type for a batch that printed none', () => {
      const timing = inferTiming([slot('refused', 0.1), slot('skipped', 0)]);
      assert.strictEqual(timing.longest_printed, undefined);
      assert.ok(!('longest_printed' in timing), 'an absent member is not written as null');
    });

    it('answers an empty batch with nothing timed', () => {
      assert.deepStrictEqual(inferTiming([]), {
        slots: 0,
        slots_ms: 0,
        first_in_file_slots: 0,
        first_in_file_ms: 0,
        slowest: [],
      });
    });

    it('joins the timings of the projects that answered one request', () => {
      const one = inferTiming([slot('a', 10, 30, true), slot('b', 400, 20)]);
      const two = inferTiming([slot('c', 90, 700, true), slot('d', 1)]);
      const joined = mergeInferTimings([one, two]);
      assert.deepStrictEqual(aliases(joined.slowest), ['b', 'c', 'a', 'd']);
      assert.strictEqual(joined.slots, 4);
      assert.strictEqual(joined.slots_ms, 501);
      assert.strictEqual(joined.first_in_file_slots, 2);
      assert.strictEqual(joined.first_in_file_ms, 100);
      assert.strictEqual(joined.longest_printed?.alias, 'c');
      // One project: what it said, unchanged.
      assert.deepStrictEqual(mergeInferTimings([one]), one);
    });

    it('caps the joined list at the same number as one batch', () => {
      const part = (prefix: string) =>
        inferTiming(Array.from({ length: SLOWEST_SLOTS }, (_, i) => slot(`${prefix}${i}`, i + 1)));
      const joined = mergeInferTimings([part('x'), part('y')]);
      assert.strictEqual(joined.slowest.length, SLOWEST_SLOTS);
      assert.strictEqual(joined.slots, 2 * SLOWEST_SLOTS);
    });
  });

  describe('the inferrer', () => {
    let root: string;
    let ordersPath: string;
    let stockPath: string;
    let batch: InferRequestItem[];

    before(() => {
      root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-infer-timing-')));
      fs.mkdirSync(path.join(root, 'src'));
      fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
      ordersPath = path.join(root, 'src', 'orders.ts');
      stockPath = path.join(root, 'src', 'stock.ts');
      fs.writeFileSync(ordersPath, ORDERS);
      fs.writeFileSync(stockPath, STOCK);
      batch = [
        { file_path: ordersPath, line_number: lineOf(ORDERS, 'function total'), infer_kind: 'signature_return', alias: 'total_return' },
        { file_path: ordersPath, line_number: lineOf(ORDERS, 'function total'), infer_kind: 'function_param', alias: 'total_discount', param_name: 'discount' },
        { file_path: ordersPath, line_number: lineOf(ORDERS, 'function label'), infer_kind: 'signature_return', alias: 'label_return' },
        { file_path: stockPath, line_number: lineOf(STOCK, 'function inStock'), infer_kind: 'signature_return', alias: 'stock_return' },
        // Skipped: plain JavaScript is not inferred.
        { file_path: path.join(root, 'src', 'plain.js'), line_number: 1, infer_kind: 'signature_return', alias: 'skipped' },
        // Not found: no function on that line.
        { file_path: stockPath, line_number: 9999, infer_kind: 'signature_return', alias: 'nowhere' },
      ];
    });

    after(() => {
      fs.rmSync(root, { recursive: true, force: true });
    });

    const inferrer = () =>
      new TypeInferrer({
        project: new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') }),
        repoRoot: root,
      });

    it('times every request of the batch, whatever the request answered', () => {
      const result = inferrer().infer(batch);
      assert.deepStrictEqual(
        (result.inferred_types ?? []).map((t) => t.alias),
        ['total_return', 'total_discount', 'label_return', 'stock_return']
      );
      const timing = result.timing;
      assert.strictEqual(timing.slots, batch.length);
      assert.deepStrictEqual(
        aliases(timing.slowest).sort(),
        ['label_return', 'nowhere', 'skipped', 'stock_return', 'total_discount', 'total_return']
      );
      for (let i = 0; i < timing.slowest.length; i += 1) {
        const named = timing.slowest[i];
        assert.ok(Number.isFinite(named.ms) && named.ms >= 0, `${named.alias} took ${named.ms}`);
        if (i > 0) assert.ok(timing.slowest[i - 1].ms >= named.ms, 'slowest first');
      }
      const sum = timing.slowest.reduce((total, named) => total + named.ms, 0);
      assert.ok(Math.abs(sum - timing.slots_ms) < 0.01, `${sum} against ${timing.slots_ms}`);
    });

    it('names each request by what was asked, and its type by its length alone', () => {
      const result = inferrer().infer(batch);
      const answered = new Map((result.inferred_types ?? []).map((t) => [t.alias, t]));
      for (const named of result.timing.slowest) {
        const request = batch.find((r) => r.alias === named.alias);
        assert.ok(request, `no request for ${named.alias}`);
        assert.deepStrictEqual(
          Object.keys(named).sort(),
          ['alias', 'file_path', 'first_in_file', 'infer_kind', 'line_number', 'ms', 'printed_length']
        );
        assert.strictEqual(named.file_path, request.file_path);
        assert.strictEqual(named.line_number, request.line_number);
        assert.strictEqual(named.infer_kind, request.infer_kind);
        // The length of the text the request answered; 0 for one that answered none.
        assert.strictEqual(named.printed_length, answered.get(named.alias!)?.type_string.length ?? 0);
      }
      const label = result.timing.slowest.find((named) => named.alias === 'label_return');
      assert.strictEqual(label?.printed_length, '{ title: string; lines: number[]; }'.length);
      assert.strictEqual(result.timing.longest_printed?.alias, 'label_return');
    });

    it('marks the first request it is asked of a file, across batches', () => {
      const one = inferrer();
      const firstOf = (timing: InferTiming) =>
        timing.slowest.filter((named) => named.first_in_file).map((named) => named.alias).sort();

      const first = one.infer(batch);
      // One per file named, the skipped and the not-found included.
      assert.deepStrictEqual(firstOf(first.timing), ['skipped', 'stock_return', 'total_return']);
      assert.strictEqual(first.timing.first_in_file_slots, 3);

      // The same inferrer has been asked of every one of those files.
      const again = one.infer(batch);
      assert.deepStrictEqual(firstOf(again.timing), []);
      assert.strictEqual(again.timing.first_in_file_slots, 0);
      assert.strictEqual(again.timing.first_in_file_ms, 0);
    });

    it('keeps the timing beside the answers, never in them', () => {
      const result = inferrer().infer(batch);
      for (const answer of result.inferred_types ?? []) {
        assert.ok(!('ms' in answer) && !('printed_length' in answer) && !('first_in_file' in answer));
      }
      // The text of a type is in the answer and nowhere in the timing.
      const written = JSON.stringify(result.timing);
      for (const answer of result.inferred_types ?? []) {
        if (answer.type_string.length < 8) continue;
        assert.ok(!written.includes(answer.type_string), `the timing writes ${answer.type_string}`);
      }
    });
  });

  describe('over stdio', () => {
    interface Frame {
      request_id: string;
      status: string;
      inferred_types?: Array<{ alias: string; type_string: string }>;
      infer_timing?: InferTiming;
      errors?: string[];
    }

    /** Send `requests` to a fresh sidecar, one at a time, and return each terminal frame. */
    const answers = async (requests: Array<Record<string, unknown>>): Promise<Frame[]> => {
      const child: ChildProcessWithoutNullStreams = spawn('node', [SIDECAR_PATH], {
        stdio: ['pipe', 'pipe', 'pipe'],
      });
      child.stderr.resume();
      const lines = readline.createInterface({ input: child.stdout })[Symbol.asyncIterator]();
      const out: Frame[] = [];
      try {
        for (const request of requests) {
          child.stdin.write(JSON.stringify(request) + '\n');
          for (;;) {
            const next = await lines.next();
            assert.ok(!next.done, `the sidecar closed before answering ${String(request.request_id)}`);
            if (!next.value.trim()) continue;
            const frame = JSON.parse(next.value) as Frame;
            if (frame.request_id !== request.request_id || frame.status === 'progress') continue;
            out.push(frame);
            break;
          }
        }
      } finally {
        child.kill();
      }
      return out;
    };

    it('the answer of an infer request carries the timing of its requests', async () => {
      const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-infer-timing-io-')));
      try {
        fs.mkdirSync(path.join(root, 'src'));
        fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
        const orders = path.join(root, 'src', 'orders.ts');
        fs.writeFileSync(orders, ORDERS);
        const requests: InferRequestItem[] = [
          { file_path: orders, line_number: lineOf(ORDERS, 'function total'), infer_kind: 'signature_return', alias: 'total_return' },
          { file_path: orders, line_number: lineOf(ORDERS, 'function label'), infer_kind: 'signature_return', alias: 'label_return' },
        ];
        const [ready, first, second] = await answers([
          { action: 'init', request_id: 'timing-init', repo_root: root },
          { action: 'infer', request_id: 'timing-1', requests },
          { action: 'infer', request_id: 'timing-2', requests },
        ]);
        assert.strictEqual(ready.status, 'ready');
        assert.strictEqual(first.status, 'success', JSON.stringify(first.errors));
        const timing = first.infer_timing;
        assert.ok(timing, 'the answer carries no timing');
        assert.strictEqual(timing.slots, 2);
        assert.deepStrictEqual(aliases(timing.slowest).sort(), ['label_return', 'total_return']);
        assert.strictEqual(timing.first_in_file_slots, 1);
        assert.strictEqual(timing.longest_printed?.alias, 'label_return');
        // The second request finds the file already asked of.
        assert.strictEqual(second.infer_timing?.first_in_file_slots, 0);
        // The timing changes nothing the request answers.
        assert.deepStrictEqual(second.inferred_types, first.inferred_types);
      } finally {
        fs.rmSync(root, { recursive: true, force: true });
      }
    });

    it('a request two projects answer carries one timing for both', async () => {
      // A solution tsconfig: each file is typed by the project that owns it.
      const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-infer-timing-two-')));
      try {
        const project = (name: string, source: string): string => {
          const dir = path.join(root, name);
          fs.mkdirSync(path.join(dir, 'src'), { recursive: true });
          fs.writeFileSync(
            path.join(dir, 'tsconfig.json'),
            JSON.stringify({
              compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, composite: true, types: [] },
              include: ['src/**/*.ts'],
            })
          );
          const file = path.join(dir, 'src', `${name}.ts`);
          fs.writeFileSync(file, source);
          return file;
        };
        const orders = project('orders', ORDERS);
        const stock = project('stock', STOCK);
        fs.writeFileSync(
          path.join(root, 'tsconfig.json'),
          JSON.stringify({ files: [], references: [{ path: './orders' }, { path: './stock' }] })
        );
        const requests: InferRequestItem[] = [
          { file_path: orders, line_number: lineOf(ORDERS, 'function label'), infer_kind: 'signature_return', alias: 'label_return' },
          { file_path: stock, line_number: lineOf(STOCK, 'function inStock'), infer_kind: 'signature_return', alias: 'stock_return' },
          { file_path: orders, line_number: lineOf(ORDERS, 'function total'), infer_kind: 'signature_return', alias: 'total_return' },
        ];
        const [ready, answer] = await answers([
          { action: 'init', request_id: 'two-init', repo_root: root, tsconfig_path: 'tsconfig.json' },
          { action: 'infer', request_id: 'two-1', requests },
        ]);
        assert.strictEqual(ready.status, 'ready');
        assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
        assert.deepStrictEqual(
          (answer.inferred_types ?? []).map((t) => t.alias).sort(),
          ['label_return', 'stock_return', 'total_return']
        );
        const timing = answer.infer_timing;
        assert.ok(timing, 'the answer carries no timing');
        assert.strictEqual(timing.slots, 3);
        assert.deepStrictEqual(aliases(timing.slowest).sort(), ['label_return', 'stock_return', 'total_return']);
        assert.strictEqual(timing.first_in_file_slots, 2);
        for (let i = 1; i < timing.slowest.length; i += 1) {
          assert.ok(timing.slowest[i - 1].ms >= timing.slowest[i].ms, 'slowest first, across the two projects');
        }
      } finally {
        fs.rmSync(root, { recursive: true, force: true });
      }
    });
  });
});
