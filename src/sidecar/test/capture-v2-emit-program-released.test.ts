/**
 * A capture holds none of its earlier compiler programs when the self-check
 * starts (carrick#1916).
 *
 * A capture builds three programs one after another: the one the anchors are
 * read in, the one that emits the declarations, and the self-check's, over
 * the emitted stub. Each is as large as the service, and the self-check is
 * the stage in which every capture measured to run out of heap died.
 *
 * The emit's program used to live as long as the capture. The emit ran inside
 * the capture's one long function, and that function's frame went on holding
 * what the emit had built while the self-check loaded its own program beside
 * it. On a 2,345-file service that was 1,290 MB of a 2,741 MB peak, the
 * largest single part of it, and nothing read it again.
 *
 * So the property is read where it costs: at the report a capture makes as
 * the self-check starts, after a forced collection, the heap holds what it
 * held before the first program was built, and not a program more.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as v8 from 'node:v8';
import * as vm from 'node:vm';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAnchorRequest, CapturePhase } from '../src/capture/api.js';

/** A full collection, asked of the engine itself: the test runner starts no process with one exposed. */
const collect: () => void = (() => {
  v8.setFlagsFromString('--expose-gc');
  return vm.runInNewContext('gc') as () => void;
})();

const MB = 1024 * 1024;

describe('carrick#1916: a capture holds no earlier program when the self-check starts', () => {
  let root: string;

  before(() => {
    root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1916-released-')));
    fs.mkdirSync(path.join(root, 'src'), { recursive: true });
    fs.writeFileSync(path.join(root, 'package.json'), JSON.stringify({ name: 'ledger', version: '1.0.0' }));
    // No `lib`: the program holds the default library, browser types and
    // all, which is what makes a program of any service tens of megabytes.
    fs.writeFileSync(
      path.join(root, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { target: 'es2022', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
        include: ['src/**/*.ts'],
      })
    );
    fs.writeFileSync(
      path.join(root, 'src', 'ledger.ts'),
      ['export interface Entry { id: string; amount: number }', 'export function entries(): Entry[] { return []; }', ''].join('\n')
    );
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const anchors: CaptureAnchorRequest[] = [
    { kind: 'symbol', alias: 'Capture_Entry', symbol_name: 'Entry', source_file: 'src/ledger.ts', anchor_origin: 'llm-symbol' },
    { kind: 'handler_return', alias: 'Capture_Entries', symbol_name: 'entries', source_file: 'src/ledger.ts', anchor_origin: 'llm-symbol' },
  ];

  /** One capture, and the heap left after a full collection at the first report of each stage. */
  const liveAtEachStage = (outDir: string): Partial<Record<CapturePhase, number>> => {
    const live: Partial<Record<CapturePhase, number>> = {};
    const result = captureStub({
      repoRoot: root,
      serviceName: 'ledger',
      anchors,
      outDir,
      onProgress: (phase) => {
        if (live[phase] !== undefined) return;
        collect();
        live[phase] = process.memoryUsage().heapUsed;
      },
    });
    assert.ok(result.success, JSON.stringify(result.errors));
    return live;
  };

  it('holds, as the self-check starts, what it held before the first program was built', () => {
    const out = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1916-released-stub-'));
    try {
      // Once unmeasured: the compiler's own code and tables are loaded by its
      // first use and stay, and they are not what a capture holds.
      liveAtEachStage(path.join(out, 'warm'));
      // Then three captures, and the least any of them held. A capture that
      // keeps a program keeps it every time; the engine compiling a function
      // in the background can keep one alive for an instant, once.
      const runs = ['a', 'b', 'c'].map((name) => {
        const live = liveAtEachStage(path.join(out, name));
        const before = live.program;
        const atSelfCheck = live['self-check'];
        assert.ok(before !== undefined && atSelfCheck !== undefined, JSON.stringify(live));
        return { heldMb: (atSelfCheck - before) / MB, live };
      });
      const least = runs.reduce((a, b) => (a.heldMb <= b.heldMb ? a : b));
      // One program of this service, held, is some 40 MB; released, the
      // capture holds its anchors and the emitted text, well under 1 MB.
      assert.ok(
        least.heldMb < 8,
        `the capture holds ${least.heldMb.toFixed(1)} MB more as the self-check starts than before it built a program: ` +
          JSON.stringify(Object.fromEntries(Object.entries(least.live).map(([phase, bytes]) => [phase, Math.round(bytes / MB)])))
      );
    } finally {
      fs.rmSync(out, { recursive: true, force: true });
    }
  });
});
